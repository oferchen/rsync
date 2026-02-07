//! Error recovery and partial transfer handling for rsync protocol operations.
//!
//! This module provides error recovery strategies, partial transfer tracking, and exit code
//! mapping that match upstream rsync's receiver.c error handling behavior.
//!
//! # Design
//!
//! The module is organized around three key concepts:
//!
//! 1. **Error Classification** - Categorizing errors into recoverable, fatal, and transient
//! 2. **Partial Transfer Tracking** - Recording incomplete transfers for potential resume
//! 3. **Recovery Strategy** - Determining the appropriate action for each error
//!
//! # Examples
//!
//! ```
//! use protocol::error_recovery::{TransferError, classify_error, should_retry};
//! use std::io;
//!
//! // Classify an I/O error
//! let err = io::Error::from(io::ErrorKind::ConnectionReset);
//! let severity = classify_error(err);
//!
//! // Determine if retry is appropriate
//! let transfer_err = TransferError::Timeout;
//! assert!(should_retry(&transfer_err, 1, 3));
//! ```

use std::collections::HashMap;
use std::io;
use std::path::PathBuf;

/// Transfer errors that can occur during rsync operations.
///
/// These error types correspond to different failure modes during file transfer,
/// each requiring specific handling strategies.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TransferError {
    /// I/O error during transfer.
    Io(io::ErrorKind),
    /// Network or I/O timeout.
    Timeout,
    /// Protocol version or capability mismatch.
    ProtocolMismatch,
    /// Checksum verification failed.
    ChecksumMismatch,
    /// Permission denied for file operation.
    PermissionDenied,
    /// Disk full or quota exceeded.
    DiskFull,
    /// Connection lost during transfer.
    ConnectionLost,
    /// Transfer interrupted by signal.
    Interrupted,
}

/// Severity classification for transfer errors.
///
/// This determines how the error should be handled:
/// - **Recoverable**: Skip the file and continue with others
/// - **Fatal**: Abort the entire transfer immediately
/// - **Transient**: Retry the operation (may succeed on retry)
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ErrorSeverity {
    /// Error affects only the current file; transfer can continue.
    Recoverable,
    /// Critical error requiring immediate abort.
    Fatal,
    /// Temporary error that may succeed on retry.
    Transient,
}

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

/// Classifies an I/O error into a severity level.
///
/// This matches upstream rsync's error classification in receiver.c, determining whether
/// an error should cause the transfer to abort, be retried, or allow skipping the file.
///
/// # Examples
///
/// ```
/// use protocol::error_recovery::{classify_error, ErrorSeverity};
/// use std::io;
///
/// let err = io::Error::from(io::ErrorKind::PermissionDenied);
/// assert_eq!(classify_error(err), ErrorSeverity::Recoverable);
///
/// let err = io::Error::from(io::ErrorKind::ConnectionReset);
/// assert_eq!(classify_error(err), ErrorSeverity::Fatal);
/// ```
pub fn classify_error(error: io::Error) -> ErrorSeverity {
    use io::ErrorKind::*;

    match error.kind() {
        // Transient errors - worth retrying
        Interrupted | WouldBlock => ErrorSeverity::Transient,

        // Recoverable errors - skip file and continue
        NotFound | PermissionDenied | AlreadyExists | InvalidInput => ErrorSeverity::Recoverable,

        // Fatal errors - abort transfer
        UnexpectedEof | InvalidData | ConnectionRefused | ConnectionReset | ConnectionAborted
        | BrokenPipe | AddrInUse | AddrNotAvailable | NotConnected | TimedOut
        | ReadOnlyFilesystem | StorageFull => ErrorSeverity::Fatal,

        // Default to fatal for unknown errors
        _ => ErrorSeverity::Fatal,
    }
}

/// Determines if a transfer error should be retried.
///
/// This implements retry logic matching upstream rsync's behavior, considering the error type,
/// number of attempts already made, and maximum retry limit.
///
/// # Arguments
///
/// * `error` - The transfer error that occurred
/// * `attempt` - The current attempt number (1-based)
/// * `max_retries` - Maximum number of retries allowed
///
/// # Examples
///
/// ```
/// use protocol::error_recovery::{TransferError, should_retry};
///
/// // Transient errors should be retried
/// let err = TransferError::Timeout;
/// assert!(should_retry(&err, 1, 3));
/// assert!(should_retry(&err, 2, 3));
/// assert!(!should_retry(&err, 3, 3)); // Exceeded max retries
///
/// // Fatal errors should not be retried
/// let err = TransferError::DiskFull;
/// assert!(!should_retry(&err, 1, 3));
/// ```
pub fn should_retry(error: &TransferError, attempt: u32, max_retries: u32) -> bool {
    // Don't retry if we've exceeded the limit
    if attempt >= max_retries {
        return false;
    }

    // Determine if the error type is retryable
    match error {
        // Transient errors worth retrying
        TransferError::Timeout
        | TransferError::ConnectionLost
        | TransferError::Interrupted
        | TransferError::Io(io::ErrorKind::Interrupted)
        | TransferError::Io(io::ErrorKind::WouldBlock)
        | TransferError::Io(io::ErrorKind::TimedOut) => true,

        // Fatal errors that should not be retried
        TransferError::DiskFull
        | TransferError::ProtocolMismatch
        | TransferError::ChecksumMismatch
        | TransferError::PermissionDenied => false,

        // Other I/O errors - check if they're transient
        TransferError::Io(kind) => {
            matches!(kind, io::ErrorKind::Interrupted | io::ErrorKind::WouldBlock)
        }
    }
}

/// Action to take in response to a transfer error.
///
/// This determines the recovery strategy based on error type and partial transfer state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RecoveryAction {
    /// Retry the entire transfer from the beginning.
    Retry,
    /// Skip this file and continue with the next.
    Skip,
    /// Abort the entire transfer immediately.
    Abort,
    /// Resume transfer from the specified byte offset.
    ResumeFrom(u64),
}

/// Determines the appropriate recovery action for a transfer error.
///
/// This matches upstream rsync's recovery strategy in receiver.c, considering both the
/// error type and whether a partial transfer exists.
///
/// # Examples
///
/// ```
/// use protocol::error_recovery::{TransferError, PartialTransferState, RecoveryAction, determine_recovery};
/// use std::path::PathBuf;
///
/// // Timeout with partial transfer - should resume
/// let error = TransferError::Timeout;
/// let partial = PartialTransferState::new(PathBuf::from("/tmp/file.txt"), 1024, 2048, None);
/// assert_eq!(determine_recovery(&error, Some(&partial)), RecoveryAction::ResumeFrom(1024));
///
/// // Permission denied - skip the file
/// let error = TransferError::PermissionDenied;
/// assert_eq!(determine_recovery(&error, None), RecoveryAction::Skip);
///
/// // Disk full - abort immediately
/// let error = TransferError::DiskFull;
/// assert_eq!(determine_recovery(&error, None), RecoveryAction::Abort);
/// ```
pub fn determine_recovery(
    error: &TransferError,
    partial: Option<&PartialTransferState>,
) -> RecoveryAction {
    match error {
        // Fatal errors require abort
        TransferError::DiskFull | TransferError::ProtocolMismatch => RecoveryAction::Abort,

        // Checksum mismatch - retry from beginning
        TransferError::ChecksumMismatch => RecoveryAction::Retry,

        // Permission denied - skip this file
        TransferError::PermissionDenied => RecoveryAction::Skip,

        // Transient errors - resume if partial exists, otherwise retry
        TransferError::Timeout | TransferError::ConnectionLost | TransferError::Interrupted => {
            if let Some(state) = partial {
                if state.is_resumable() {
                    RecoveryAction::ResumeFrom(state.bytes_received)
                } else {
                    RecoveryAction::Retry
                }
            } else {
                RecoveryAction::Retry
            }
        }

        // I/O errors - classify and decide
        TransferError::Io(kind) => match kind {
            io::ErrorKind::NotFound | io::ErrorKind::PermissionDenied => RecoveryAction::Skip,
            io::ErrorKind::StorageFull => RecoveryAction::Abort,
            io::ErrorKind::Interrupted | io::ErrorKind::WouldBlock => {
                if let Some(state) = partial {
                    if state.is_resumable() {
                        RecoveryAction::ResumeFrom(state.bytes_received)
                    } else {
                        RecoveryAction::Retry
                    }
                } else {
                    RecoveryAction::Retry
                }
            }
            _ => RecoveryAction::Abort,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // PartialTransferState tests

    #[test]
    fn partial_transfer_state_new() {
        let state = PartialTransferState::new(PathBuf::from("/tmp/file.txt"), 1024, 2048, None);
        assert_eq!(state.path, PathBuf::from("/tmp/file.txt"));
        assert_eq!(state.bytes_received, 1024);
        assert_eq!(state.expected_size, 2048);
        assert!(state.checksum_so_far.is_none());
    }

    #[test]
    fn partial_transfer_state_is_resumable() {
        // Partially received - should be resumable
        let state = PartialTransferState::new(PathBuf::from("/tmp/file.txt"), 1024, 2048, None);
        assert!(state.is_resumable());

        // No data received - not resumable
        let state = PartialTransferState::new(PathBuf::from("/tmp/file.txt"), 0, 2048, None);
        assert!(!state.is_resumable());

        // Fully received - not resumable
        let state = PartialTransferState::new(PathBuf::from("/tmp/file.txt"), 2048, 2048, None);
        assert!(!state.is_resumable());

        // Received more than expected (should not happen, but handle gracefully)
        let state = PartialTransferState::new(PathBuf::from("/tmp/file.txt"), 3000, 2048, None);
        assert!(!state.is_resumable());
    }

    #[test]
    fn partial_transfer_state_bytes_remaining() {
        let state = PartialTransferState::new(PathBuf::from("/tmp/file.txt"), 1024, 2048, None);
        assert_eq!(state.bytes_remaining(), 1024);

        let state = PartialTransferState::new(PathBuf::from("/tmp/file.txt"), 0, 2048, None);
        assert_eq!(state.bytes_remaining(), 2048);

        let state = PartialTransferState::new(PathBuf::from("/tmp/file.txt"), 2048, 2048, None);
        assert_eq!(state.bytes_remaining(), 0);
    }

    #[test]
    fn partial_transfer_state_with_checksum() {
        let checksum = vec![0x01, 0x02, 0x03, 0x04];
        let state = PartialTransferState::new(
            PathBuf::from("/tmp/file.txt"),
            1024,
            2048,
            Some(checksum.clone()),
        );
        assert_eq!(state.checksum_so_far, Some(checksum));
    }

    // PartialTransferLog tests

    #[test]
    fn partial_transfer_log_new() {
        let log = PartialTransferLog::new();
        assert_eq!(log.count(), 0);
    }

    #[test]
    fn partial_transfer_log_record_and_get() {
        let mut log = PartialTransferLog::new();
        let path = PathBuf::from("/tmp/file.txt");
        let state = PartialTransferState::new(path.clone(), 1024, 2048, None);

        log.record_partial(state);
        assert_eq!(log.count(), 1);

        let retrieved = log.get_resumable(&path);
        assert!(retrieved.is_some());
        assert_eq!(retrieved.unwrap().bytes_received, 1024);
    }

    #[test]
    fn partial_transfer_log_get_non_resumable() {
        let mut log = PartialTransferLog::new();
        let path = PathBuf::from("/tmp/file.txt");

        // Record a non-resumable state (0 bytes received)
        let state = PartialTransferState::new(path.clone(), 0, 2048, None);
        log.record_partial(state);

        // Should return None since it's not resumable
        assert!(log.get_resumable(&path).is_none());
    }

    #[test]
    fn partial_transfer_log_replace_existing() {
        let mut log = PartialTransferLog::new();
        let path = PathBuf::from("/tmp/file.txt");

        // Record first state
        let state1 = PartialTransferState::new(path.clone(), 1024, 2048, None);
        log.record_partial(state1);

        // Record second state for same path
        let state2 = PartialTransferState::new(path.clone(), 1536, 2048, None);
        log.record_partial(state2);

        // Should have replaced, not added
        assert_eq!(log.count(), 1);
        assert_eq!(log.get_resumable(&path).unwrap().bytes_received, 1536);
    }

    #[test]
    fn partial_transfer_log_clear() {
        let mut log = PartialTransferLog::new();
        let state = PartialTransferState::new(PathBuf::from("/tmp/file.txt"), 1024, 2048, None);
        log.record_partial(state);
        assert_eq!(log.count(), 1);

        log.clear();
        assert_eq!(log.count(), 0);
    }

    #[test]
    fn partial_transfer_log_iter() {
        let mut log = PartialTransferLog::new();
        log.record_partial(PartialTransferState::new(
            PathBuf::from("/tmp/file1.txt"),
            1024,
            2048,
            None,
        ));
        log.record_partial(PartialTransferState::new(
            PathBuf::from("/tmp/file2.txt"),
            512,
            1024,
            None,
        ));

        let count = log.iter().count();
        assert_eq!(count, 2);

        let paths: Vec<_> = log.iter().map(|(p, _)| p.clone()).collect();
        assert!(paths.contains(&PathBuf::from("/tmp/file1.txt")));
        assert!(paths.contains(&PathBuf::from("/tmp/file2.txt")));
    }

    // Error classification tests

    #[test]
    fn classify_transient_errors() {
        assert_eq!(
            classify_error(io::Error::from(io::ErrorKind::Interrupted)),
            ErrorSeverity::Transient
        );
        assert_eq!(
            classify_error(io::Error::from(io::ErrorKind::WouldBlock)),
            ErrorSeverity::Transient
        );
    }

    #[test]
    fn classify_recoverable_errors() {
        assert_eq!(
            classify_error(io::Error::from(io::ErrorKind::NotFound)),
            ErrorSeverity::Recoverable
        );
        assert_eq!(
            classify_error(io::Error::from(io::ErrorKind::PermissionDenied)),
            ErrorSeverity::Recoverable
        );
        assert_eq!(
            classify_error(io::Error::from(io::ErrorKind::AlreadyExists)),
            ErrorSeverity::Recoverable
        );
        assert_eq!(
            classify_error(io::Error::from(io::ErrorKind::InvalidInput)),
            ErrorSeverity::Recoverable
        );
    }

    #[test]
    fn classify_fatal_errors() {
        assert_eq!(
            classify_error(io::Error::from(io::ErrorKind::UnexpectedEof)),
            ErrorSeverity::Fatal
        );
        assert_eq!(
            classify_error(io::Error::from(io::ErrorKind::InvalidData)),
            ErrorSeverity::Fatal
        );
        assert_eq!(
            classify_error(io::Error::from(io::ErrorKind::ConnectionReset)),
            ErrorSeverity::Fatal
        );
        assert_eq!(
            classify_error(io::Error::from(io::ErrorKind::BrokenPipe)),
            ErrorSeverity::Fatal
        );
        assert_eq!(
            classify_error(io::Error::from(io::ErrorKind::TimedOut)),
            ErrorSeverity::Fatal
        );
        assert_eq!(
            classify_error(io::Error::from(io::ErrorKind::StorageFull)),
            ErrorSeverity::Fatal
        );
    }

    #[test]
    fn classify_unknown_error_as_fatal() {
        assert_eq!(
            classify_error(io::Error::from(io::ErrorKind::Other)),
            ErrorSeverity::Fatal
        );
    }

    // Retry logic tests

    #[test]
    fn should_retry_timeout() {
        let err = TransferError::Timeout;
        assert!(should_retry(&err, 1, 3));
        assert!(should_retry(&err, 2, 3));
        assert!(!should_retry(&err, 3, 3)); // At limit
        assert!(!should_retry(&err, 4, 3)); // Exceeded
    }

    #[test]
    fn should_retry_connection_lost() {
        let err = TransferError::ConnectionLost;
        assert!(should_retry(&err, 1, 3));
        assert!(should_retry(&err, 2, 3));
    }

    #[test]
    fn should_retry_interrupted() {
        let err = TransferError::Interrupted;
        assert!(should_retry(&err, 1, 3));
    }

    #[test]
    fn should_not_retry_disk_full() {
        let err = TransferError::DiskFull;
        assert!(!should_retry(&err, 1, 3));
    }

    #[test]
    fn should_not_retry_protocol_mismatch() {
        let err = TransferError::ProtocolMismatch;
        assert!(!should_retry(&err, 1, 3));
    }

    #[test]
    fn should_not_retry_checksum_mismatch() {
        let err = TransferError::ChecksumMismatch;
        assert!(!should_retry(&err, 1, 3));
    }

    #[test]
    fn should_not_retry_permission_denied() {
        let err = TransferError::PermissionDenied;
        assert!(!should_retry(&err, 1, 3));
    }

    #[test]
    fn should_retry_io_interrupted() {
        let err = TransferError::Io(io::ErrorKind::Interrupted);
        assert!(should_retry(&err, 1, 3));
    }

    #[test]
    fn should_not_retry_io_not_found() {
        let err = TransferError::Io(io::ErrorKind::NotFound);
        assert!(!should_retry(&err, 1, 3));
    }

    #[test]
    fn should_not_retry_when_at_max() {
        let err = TransferError::Timeout;
        assert!(!should_retry(&err, 5, 5));
        assert!(!should_retry(&err, 10, 5));
    }

    // Recovery action tests

    #[test]
    fn determine_recovery_disk_full_aborts() {
        let err = TransferError::DiskFull;
        assert_eq!(determine_recovery(&err, None), RecoveryAction::Abort);
    }

    #[test]
    fn determine_recovery_protocol_mismatch_aborts() {
        let err = TransferError::ProtocolMismatch;
        assert_eq!(determine_recovery(&err, None), RecoveryAction::Abort);
    }

    #[test]
    fn determine_recovery_checksum_mismatch_retries() {
        let err = TransferError::ChecksumMismatch;
        assert_eq!(determine_recovery(&err, None), RecoveryAction::Retry);
    }

    #[test]
    fn determine_recovery_permission_denied_skips() {
        let err = TransferError::PermissionDenied;
        assert_eq!(determine_recovery(&err, None), RecoveryAction::Skip);
    }

    #[test]
    fn determine_recovery_timeout_without_partial_retries() {
        let err = TransferError::Timeout;
        assert_eq!(determine_recovery(&err, None), RecoveryAction::Retry);
    }

    #[test]
    fn determine_recovery_timeout_with_partial_resumes() {
        let err = TransferError::Timeout;
        let partial = PartialTransferState::new(PathBuf::from("/tmp/file.txt"), 1024, 2048, None);
        assert_eq!(
            determine_recovery(&err, Some(&partial)),
            RecoveryAction::ResumeFrom(1024)
        );
    }

    #[test]
    fn determine_recovery_timeout_with_non_resumable_partial_retries() {
        let err = TransferError::Timeout;
        let partial = PartialTransferState::new(PathBuf::from("/tmp/file.txt"), 0, 2048, None);
        assert_eq!(
            determine_recovery(&err, Some(&partial)),
            RecoveryAction::Retry
        );
    }

    #[test]
    fn determine_recovery_io_not_found_skips() {
        let err = TransferError::Io(io::ErrorKind::NotFound);
        assert_eq!(determine_recovery(&err, None), RecoveryAction::Skip);
    }

    #[test]
    fn determine_recovery_io_storage_full_aborts() {
        let err = TransferError::Io(io::ErrorKind::StorageFull);
        assert_eq!(determine_recovery(&err, None), RecoveryAction::Abort);
    }

    #[test]
    fn determine_recovery_io_interrupted_with_partial_resumes() {
        let err = TransferError::Io(io::ErrorKind::Interrupted);
        let partial = PartialTransferState::new(PathBuf::from("/tmp/file.txt"), 512, 1024, None);
        assert_eq!(
            determine_recovery(&err, Some(&partial)),
            RecoveryAction::ResumeFrom(512)
        );
    }

    #[test]
    fn determine_recovery_io_other_aborts() {
        let err = TransferError::Io(io::ErrorKind::Other);
        assert_eq!(determine_recovery(&err, None), RecoveryAction::Abort);
    }

    // Edge cases

    #[test]
    fn partial_transfer_state_empty_path() {
        let state = PartialTransferState::new(PathBuf::new(), 1024, 2048, None);
        assert_eq!(state.path, PathBuf::new());
    }

    #[test]
    fn partial_transfer_state_zero_expected_size() {
        let state = PartialTransferState::new(PathBuf::from("/tmp/file.txt"), 0, 0, None);
        assert!(!state.is_resumable());
        assert_eq!(state.bytes_remaining(), 0);
    }

    #[test]
    fn should_retry_with_zero_max_retries() {
        let err = TransferError::Timeout;
        assert!(!should_retry(&err, 0, 0));
        assert!(!should_retry(&err, 1, 0));
    }

    #[test]
    fn partial_transfer_log_get_nonexistent_path() {
        let log = PartialTransferLog::new();
        let path = PathBuf::from("/tmp/nonexistent.txt");
        assert!(log.get_resumable(&path).is_none());
    }
}
