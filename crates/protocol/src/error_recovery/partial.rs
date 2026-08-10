//! Partial transfer tracking for resume operations.
//!
//! Records incomplete transfers so they can be retried or resumed later,
//! matching upstream rsync's partial transfer handling.

use std::collections::HashMap;
use std::path::PathBuf;

/// State of a partially transferred file.
///
/// This tracks enough information to potentially resume a transfer from where it left off.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PartialTransferState {
    /// Path to the partially transferred file.
    pub path: PathBuf,
    /// Number of bytes successfully received so far.
    pub bytes_received: u64,
    /// Expected total size of the file.
    pub expected_size: u64,
    /// Checksum of data received so far (if available).
    pub checksum_so_far: Option<Vec<u8>>,
}

impl PartialTransferState {
    /// Creates a new partial transfer state.
    ///
    /// # Examples
    ///
    /// ```
    /// use protocol::error_recovery::PartialTransferState;
    /// use std::path::PathBuf;
    ///
    /// let state = PartialTransferState::new(
    ///     PathBuf::from("/tmp/file.txt"),
    ///     1024,
    ///     2048,
    ///     None,
    /// );
    /// assert_eq!(state.bytes_received, 1024);
    /// ```
    #[must_use]
    pub fn new(
        path: PathBuf,
        bytes_received: u64,
        expected_size: u64,
        checksum_so_far: Option<Vec<u8>>,
    ) -> Self {
        Self {
            path,
            bytes_received,
            expected_size,
            checksum_so_far,
        }
    }

    /// Returns true if this transfer is resumable (has received some data but not all).
    ///
    /// # Examples
    ///
    /// ```
    /// use protocol::error_recovery::PartialTransferState;
    /// use std::path::PathBuf;
    ///
    /// let state = PartialTransferState::new(PathBuf::from("/tmp/file.txt"), 1024, 2048, None);
    /// assert!(state.is_resumable());
    ///
    /// let complete = PartialTransferState::new(PathBuf::from("/tmp/file.txt"), 2048, 2048, None);
    /// assert!(!complete.is_resumable());
    ///
    /// let empty = PartialTransferState::new(PathBuf::from("/tmp/file.txt"), 0, 2048, None);
    /// assert!(!empty.is_resumable());
    /// ```
    #[must_use]
    pub fn is_resumable(&self) -> bool {
        self.bytes_received > 0 && self.bytes_received < self.expected_size
    }

    /// Returns the number of bytes remaining to transfer.
    ///
    /// # Examples
    ///
    /// ```
    /// use protocol::error_recovery::PartialTransferState;
    /// use std::path::PathBuf;
    ///
    /// let state = PartialTransferState::new(PathBuf::from("/tmp/file.txt"), 1024, 2048, None);
    /// assert_eq!(state.bytes_remaining(), 1024);
    /// ```
    #[must_use]
    pub fn bytes_remaining(&self) -> u64 {
        self.expected_size.saturating_sub(self.bytes_received)
    }
}

/// Log of partial transfers for potential resume operations.
///
/// This accumulates records of incomplete transfers so they can be retried or resumed later.
#[derive(Debug, Default)]
pub struct PartialTransferLog {
    entries: HashMap<PathBuf, PartialTransferState>,
}

impl PartialTransferLog {
    /// Creates a new empty partial transfer log.
    ///
    /// # Examples
    ///
    /// ```
    /// use protocol::error_recovery::PartialTransferLog;
    ///
    /// let log = PartialTransferLog::new();
    /// assert_eq!(log.count(), 0);
    /// ```
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Records a partial transfer state.
    ///
    /// If a record for this path already exists, it is replaced.
    ///
    /// # Examples
    ///
    /// ```
    /// use protocol::error_recovery::{PartialTransferLog, PartialTransferState};
    /// use std::path::PathBuf;
    ///
    /// let mut log = PartialTransferLog::new();
    /// let state = PartialTransferState::new(PathBuf::from("/tmp/file.txt"), 1024, 2048, None);
    /// log.record_partial(state);
    /// assert_eq!(log.count(), 1);
    /// ```
    pub fn record_partial(&mut self, state: PartialTransferState) {
        self.entries.insert(state.path.clone(), state);
    }

    /// Gets a resumable partial transfer for the given path, if one exists.
    ///
    /// Returns `None` if there is no record for this path or if the transfer is not resumable.
    ///
    /// # Examples
    ///
    /// ```
    /// use protocol::error_recovery::{PartialTransferLog, PartialTransferState};
    /// use std::path::PathBuf;
    ///
    /// let mut log = PartialTransferLog::new();
    /// let path = PathBuf::from("/tmp/file.txt");
    /// let state = PartialTransferState::new(path.clone(), 1024, 2048, None);
    /// log.record_partial(state);
    ///
    /// let resumable = log.get_resumable(&path);
    /// assert!(resumable.is_some());
    /// assert_eq!(resumable.unwrap().bytes_received, 1024);
    /// ```
    pub fn get_resumable(&self, path: &PathBuf) -> Option<&PartialTransferState> {
        self.entries.get(path).filter(|s| s.is_resumable())
    }

    /// Returns the number of partial transfers recorded.
    ///
    /// # Examples
    ///
    /// ```
    /// use protocol::error_recovery::{PartialTransferLog, PartialTransferState};
    /// use std::path::PathBuf;
    ///
    /// let mut log = PartialTransferLog::new();
    /// assert_eq!(log.count(), 0);
    ///
    /// let state = PartialTransferState::new(PathBuf::from("/tmp/file.txt"), 1024, 2048, None);
    /// log.record_partial(state);
    /// assert_eq!(log.count(), 1);
    /// ```
    #[must_use]
    pub fn count(&self) -> usize {
        self.entries.len()
    }

    /// Clears all partial transfer records.
    ///
    /// # Examples
    ///
    /// ```
    /// use protocol::error_recovery::{PartialTransferLog, PartialTransferState};
    /// use std::path::PathBuf;
    ///
    /// let mut log = PartialTransferLog::new();
    /// let state = PartialTransferState::new(PathBuf::from("/tmp/file.txt"), 1024, 2048, None);
    /// log.record_partial(state);
    /// assert_eq!(log.count(), 1);
    ///
    /// log.clear();
    /// assert_eq!(log.count(), 0);
    /// ```
    pub fn clear(&mut self) {
        self.entries.clear();
    }

    /// Returns an iterator over all partial transfer states.
    ///
    /// # Examples
    ///
    /// ```
    /// use protocol::error_recovery::{PartialTransferLog, PartialTransferState};
    /// use std::path::PathBuf;
    ///
    /// let mut log = PartialTransferLog::new();
    /// log.record_partial(PartialTransferState::new(PathBuf::from("/tmp/file1.txt"), 1024, 2048, None));
    /// log.record_partial(PartialTransferState::new(PathBuf::from("/tmp/file2.txt"), 512, 1024, None));
    ///
    /// assert_eq!(log.iter().count(), 2);
    /// ```
    pub fn iter(&self) -> impl Iterator<Item = (&PathBuf, &PartialTransferState)> {
        self.entries.iter()
    }
}
