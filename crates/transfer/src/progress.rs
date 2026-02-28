//! Progress reporting for server-side transfer operations.
//!
//! Provides the [`TransferProgressCallback`] trait for receiving incremental
//! progress notifications as files are transferred. This enables callers
//! (CLI, embedding library, daemon) to display live progress indicators
//! during remote transfers over SSH or daemon connections.

use std::path::Path;

/// Progress event emitted when a file transfer completes.
///
/// Reports per-file completion along with aggregate counters that enable
/// callers to compute overall progress (e.g., "5 of 42 files").
pub struct TransferProgressEvent<'a> {
    /// Relative path of the file that was transferred.
    pub path: &'a Path,
    /// Bytes transferred for this file.
    pub file_bytes: u64,
    /// Total size of the file, if known from the file list.
    pub total_file_bytes: Option<u64>,
    /// Number of files transferred so far (including this one).
    pub files_done: usize,
    /// Total number of files to transfer.
    pub total_files: usize,
}

/// Callback trait for transfer progress reporting.
///
/// Implement this trait to receive notifications as each file completes
/// during a remote transfer. The trait is object-safe for use with
/// `dyn TransferProgressCallback`.
pub trait TransferProgressCallback {
    /// Called when a file transfer completes.
    fn on_file_transferred(&mut self, event: &TransferProgressEvent<'_>);
}

impl<F: FnMut(&TransferProgressEvent<'_>)> TransferProgressCallback for F {
    fn on_file_transferred(&mut self, event: &TransferProgressEvent<'_>) {
        self(event);
    }
}
