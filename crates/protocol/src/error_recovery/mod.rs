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
//! # Submodules
//!
//! - `types` - Error, severity, and recovery action enums
//! - `partial` - Partial transfer state and log tracking
//! - `strategy` - Error classification, retry logic, and recovery determination
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

mod partial;
mod strategy;
mod types;

pub use partial::{PartialTransferLog, PartialTransferState};
pub use strategy::{classify_error, determine_recovery, should_retry};
pub use types::{ErrorSeverity, RecoveryAction, TransferError};

#[cfg(test)]
mod tests {
    use super::*;
    use std::io;
    use std::path::PathBuf;

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
        let state = PartialTransferState::new(PathBuf::from("/tmp/file.txt"), 1024, 2048, None);
        assert!(state.is_resumable());

        let state = PartialTransferState::new(PathBuf::from("/tmp/file.txt"), 0, 2048, None);
        assert!(!state.is_resumable());

        let state = PartialTransferState::new(PathBuf::from("/tmp/file.txt"), 2048, 2048, None);
        assert!(!state.is_resumable());

        // Over-received case shouldn't happen in practice but must not panic or report resumable.
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

        let state = PartialTransferState::new(path.clone(), 0, 2048, None);
        log.record_partial(state);

        assert!(log.get_resumable(&path).is_none());
    }

    #[test]
    fn partial_transfer_log_replace_existing() {
        let mut log = PartialTransferLog::new();
        let path = PathBuf::from("/tmp/file.txt");

        let state1 = PartialTransferState::new(path.clone(), 1024, 2048, None);
        log.record_partial(state1);

        let state2 = PartialTransferState::new(path.clone(), 1536, 2048, None);
        log.record_partial(state2);

        // Re-recording the same path replaces the prior entry rather than appending a duplicate.
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

    #[test]
    fn should_retry_timeout() {
        let err = TransferError::Timeout;
        assert!(should_retry(&err, 1, 3));
        assert!(should_retry(&err, 2, 3));
        assert!(!should_retry(&err, 3, 3));
        assert!(!should_retry(&err, 4, 3));
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
