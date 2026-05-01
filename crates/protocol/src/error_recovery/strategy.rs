//! Error classification, retry logic, and recovery strategy selection.
//!
//! These functions match upstream rsync's error handling behavior in receiver.c,
//! determining how each error type should be handled.

use std::io;

use super::{ErrorSeverity, PartialTransferState, RecoveryAction, TransferError};

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
#[must_use]
pub fn classify_error(error: io::Error) -> ErrorSeverity {
    use io::ErrorKind::*;

    match error.kind() {
        Interrupted | WouldBlock => ErrorSeverity::Transient,

        NotFound | PermissionDenied | AlreadyExists | InvalidInput => ErrorSeverity::Recoverable,

        UnexpectedEof | InvalidData | ConnectionRefused | ConnectionReset | ConnectionAborted
        | BrokenPipe | AddrInUse | AddrNotAvailable | NotConnected | TimedOut
        | ReadOnlyFilesystem | StorageFull => ErrorSeverity::Fatal,

        // Unknown kinds default to Fatal so unexpected conditions abort rather than silently
        // skip - matches upstream rsync's conservative receiver.c handling.
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
#[must_use]
pub fn should_retry(error: &TransferError, attempt: u32, max_retries: u32) -> bool {
    if attempt >= max_retries {
        return false;
    }

    match error {
        TransferError::Timeout
        | TransferError::ConnectionLost
        | TransferError::Interrupted
        | TransferError::Io(io::ErrorKind::Interrupted)
        | TransferError::Io(io::ErrorKind::WouldBlock)
        | TransferError::Io(io::ErrorKind::TimedOut) => true,

        TransferError::DiskFull
        | TransferError::ProtocolMismatch
        | TransferError::ChecksumMismatch
        | TransferError::PermissionDenied => false,

        TransferError::Io(kind) => {
            matches!(kind, io::ErrorKind::Interrupted | io::ErrorKind::WouldBlock)
        }
    }
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
#[must_use]
pub fn determine_recovery(
    error: &TransferError,
    partial: Option<&PartialTransferState>,
) -> RecoveryAction {
    match error {
        TransferError::DiskFull | TransferError::ProtocolMismatch => RecoveryAction::Abort,

        TransferError::ChecksumMismatch => RecoveryAction::Retry,

        TransferError::PermissionDenied => RecoveryAction::Skip,

        // Transient failures prefer resuming from the partial offset when one exists; without
        // a resumable partial, fall back to a full retry.
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
