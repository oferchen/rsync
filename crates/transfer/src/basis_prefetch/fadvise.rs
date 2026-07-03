//! `posix_fadvise(WILLNEED)` prefetcher backed by a background worker thread.
//!
//! Basis paths are handed to a bounded channel; a single worker opens each
//! file read-only and issues [`fast_io::willneed::hint_basis_willneed`], then
//! drops the handle. The bounded channel provides backpressure so the main
//! thread never queues an unbounded backlog of hints. All errors (open,
//! hint, closed channel) are silent no-ops - the hint is best-effort.
//!
//! Uses `std::sync::mpsc::sync_channel` (bounded, in std) rather than a new
//! dependency - transfer already avoids crossbeam-channel in production code.

use std::fs::File;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{SyncSender, TrySendError, sync_channel};
use std::thread::JoinHandle;

use super::BasisPrefetcher;

/// Read-ahead prefetcher that warms basis pages via `posix_fadvise(WILLNEED)`.
pub struct FadviseWillneedPrefetcher {
    /// Bounded queue of basis paths to warm; `None` after shutdown.
    tx: Option<SyncSender<PathBuf>>,
    /// Background worker draining the queue; joined on drop.
    worker: Option<JoinHandle<()>>,
}

impl FadviseWillneedPrefetcher {
    /// Spawns the worker with a bounded queue of `depth` pending hints.
    ///
    /// # Errors
    ///
    /// Returns an error if the worker thread cannot be spawned.
    pub fn new(depth: usize) -> io::Result<Self> {
        let (tx, rx) = sync_channel::<PathBuf>(depth.max(1));
        let worker = std::thread::Builder::new()
            .name("basis-prefetch".to_string())
            .spawn(move || {
                // Drain until the sender is dropped. Each hint is best-effort.
                for path in rx.iter() {
                    if let Ok(file) = File::open(&path) {
                        let _ = fast_io::willneed::hint_basis_willneed(&file);
                    }
                }
            })?;
        Ok(Self {
            tx: Some(tx),
            worker: Some(worker),
        })
    }
}

impl BasisPrefetcher for FadviseWillneedPrefetcher {
    fn prefetch(&self, path: &Path) {
        // Non-blocking: if the queue is full the worker is already saturated,
        // so drop this hint rather than stall the pipeline's main thread.
        if let Some(tx) = self.tx.as_ref() {
            match tx.try_send(path.to_path_buf()) {
                Ok(()) | Err(TrySendError::Full(_)) | Err(TrySendError::Disconnected(_)) => {}
            }
        }
    }
}

impl Drop for FadviseWillneedPrefetcher {
    fn drop(&mut self) {
        // Close the channel so the worker's `rx.iter()` terminates, then join.
        self.tx = None;
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}
