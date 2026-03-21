//! Error types and severity classification for rsync protocol operations.
//!
//! Defines the core error and action enums used throughout error recovery.

use std::io;

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
