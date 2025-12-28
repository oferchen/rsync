use std::io;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use thiserror::Error;

use super::filter_program::{
    INVALID_OPERAND_EXIT_CODE, MAX_DELETE_EXIT_CODE, MISSING_OPERANDS_EXIT_CODE, TIMEOUT_EXIT_CODE,
};

/// Error produced when planning or executing a local copy fails.
#[derive(Debug, Error)]
#[error(transparent)]
pub struct LocalCopyError {
    #[from]
    kind: LocalCopyErrorKind,
}

impl LocalCopyError {
    const fn new(kind: LocalCopyErrorKind) -> Self {
        Self { kind }
    }

    /// Constructs an error representing missing operands.
    #[must_use]
    pub const fn missing_operands() -> Self {
        Self::new(LocalCopyErrorKind::MissingSourceOperands)
    }

    /// Constructs an invalid-argument error.
    #[must_use]
    pub const fn invalid_argument(reason: LocalCopyArgumentError) -> Self {
        Self::new(LocalCopyErrorKind::InvalidArgument(reason))
    }

    /// Constructs an error indicating that the deletion limit was exceeded.
    #[must_use]
    pub const fn delete_limit_exceeded(skipped: u64) -> Self {
        Self::new(LocalCopyErrorKind::DeleteLimitExceeded { skipped })
    }

    /// Constructs an I/O error with action context.
    #[must_use]
    pub const fn io(action: &'static str, path: PathBuf, source: io::Error) -> Self {
        Self::new(LocalCopyErrorKind::Io {
            action,
            path,
            source,
        })
    }

    /// Constructs an error representing an inactivity timeout.
    #[must_use]
    pub const fn timeout(duration: Duration) -> Self {
        Self::new(LocalCopyErrorKind::Timeout { duration })
    }

    /// Constructs an error indicating the configured stop-at deadline was reached.
    #[must_use]
    pub const fn stop_at_reached(target: SystemTime) -> Self {
        Self::new(LocalCopyErrorKind::StopAtReached { target })
    }

    /// Returns the exit code that mirrors upstream rsync's behaviour.
    #[must_use]
    pub const fn exit_code(&self) -> i32 {
        match self.kind {
            LocalCopyErrorKind::MissingSourceOperands => MISSING_OPERANDS_EXIT_CODE,
            LocalCopyErrorKind::InvalidArgument(_) | LocalCopyErrorKind::Io { .. } => {
                INVALID_OPERAND_EXIT_CODE
            }
            LocalCopyErrorKind::Timeout { .. } => TIMEOUT_EXIT_CODE,
            LocalCopyErrorKind::DeleteLimitExceeded { .. } => MAX_DELETE_EXIT_CODE,
            LocalCopyErrorKind::StopAtReached { .. } => TIMEOUT_EXIT_CODE,
        }
    }

    /// Provides access to the underlying error kind.
    #[must_use]
    pub const fn kind(&self) -> &LocalCopyErrorKind {
        &self.kind
    }

    /// Consumes the error and returns its kind.
    #[must_use]
    pub fn into_kind(self) -> LocalCopyErrorKind {
        self.kind
    }
}

/// Classification of local copy failures.
#[derive(Debug, Error)]
pub enum LocalCopyErrorKind {
    /// No operands were supplied.
    #[error("missing source operands: supply at least one source and a destination")]
    MissingSourceOperands,
    /// Operands were invalid.
    #[error("{}", .0.message())]
    InvalidArgument(LocalCopyArgumentError),
    /// Filesystem interaction failed.
    #[error("failed to {action} '{}': {source}", path.display())]
    Io {
        /// Action being performed.
        action: &'static str,
        /// Path involved in the failure.
        path: PathBuf,
        /// Underlying error.
        #[source]
        source: io::Error,
    },
    /// The transfer exceeded the configured inactivity timeout.
    #[error("transfer timed out after {:.3} seconds without progress", duration.as_secs_f64())]
    Timeout {
        /// Duration of inactivity that triggered the timeout.
        duration: Duration,
    },
    /// Deletions were halted because the configured limit was exceeded.
    #[error("Deletions stopped due to --max-delete limit ({skipped} {} skipped)", if *skipped == 1 { "entry" } else { "entries" })]
    DeleteLimitExceeded {
        /// Number of entries that were skipped after reaching the limit.
        skipped: u64,
    },
    /// The configured stop-at deadline was reached.
    #[error("stopping at requested limit")]
    StopAtReached {
        /// The requested wall-clock deadline.
        target: SystemTime,
    },
}

impl LocalCopyErrorKind {
    /// Returns the action, path, and source error for [`LocalCopyErrorKind::Io`] values.
    #[must_use]
    pub fn as_io(&self) -> Option<(&'static str, &Path, &io::Error)> {
        match self {
            Self::Io {
                action,
                path,
                source,
            } => Some((action, path.as_path(), source)),
            _ => None,
        }
    }
}

/// Detailed reason for operand validation failures.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LocalCopyArgumentError {
    /// A source operand was empty.
    EmptySourceOperand,
    /// The destination operand was empty.
    EmptyDestinationOperand,
    /// Multiple sources targeted a non-directory destination.
    DestinationMustBeDirectory,
    /// Unable to determine the directory name from the source operand.
    DirectoryNameUnavailable,
    /// Unable to determine the file name from the source operand.
    FileNameUnavailable,
    /// Unable to determine the link name from the source operand.
    LinkNameUnavailable,
    /// Encountered a file type that is unsupported.
    UnsupportedFileType,
    /// Attempted to replace an existing directory with a symbolic link.
    ReplaceDirectoryWithSymlink,
    /// Attempted to replace an existing directory with a regular file.
    ReplaceDirectoryWithFile,
    /// Attempted to replace an existing directory with a special file.
    ReplaceDirectoryWithSpecial,
    /// Attempted to replace a non-directory with a directory.
    ReplaceNonDirectoryWithDirectory,
    /// Encountered an operand that refers to a remote host or module.
    RemoteOperandUnsupported,
}

impl LocalCopyArgumentError {
    /// Returns the canonical diagnostic message associated with the error.
    #[must_use]
    pub const fn message(self) -> &'static str {
        match self {
            Self::EmptySourceOperand => "source operands must be non-empty",
            Self::EmptyDestinationOperand => "destination operand must be non-empty",
            Self::DestinationMustBeDirectory => {
                "destination must be an existing directory when copying multiple sources"
            }
            Self::DirectoryNameUnavailable => "cannot determine directory name",
            Self::FileNameUnavailable => "cannot determine file name",
            Self::LinkNameUnavailable => "cannot determine link name",
            Self::UnsupportedFileType => "unsupported file type encountered",
            Self::ReplaceDirectoryWithSymlink => {
                "cannot replace existing directory with symbolic link"
            }
            Self::ReplaceDirectoryWithFile => "cannot replace existing directory with regular file",
            Self::ReplaceDirectoryWithSpecial => {
                "cannot replace existing directory with special file"
            }
            Self::ReplaceNonDirectoryWithDirectory => {
                "cannot replace non-directory destination with directory"
            }
            Self::RemoteOperandUnsupported => concat!(
                "remote operands are not supported: this build handles local filesystem copies only; ",
                "set OC_RSYNC_FALLBACK to point to an upstream rsync binary for remote transfers",
            ),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::ErrorKind;

    #[test]
    fn local_copy_error_missing_operands() {
        let error = LocalCopyError::missing_operands();
        assert_eq!(error.exit_code(), MISSING_OPERANDS_EXIT_CODE);
        assert!(matches!(
            error.kind(),
            LocalCopyErrorKind::MissingSourceOperands
        ));
    }

    #[test]
    fn local_copy_error_missing_operands_message() {
        let error = LocalCopyError::missing_operands();
        let message = error.to_string();
        assert!(message.contains("missing source operands"));
    }

    #[test]
    fn local_copy_error_invalid_argument() {
        let error = LocalCopyError::invalid_argument(LocalCopyArgumentError::EmptySourceOperand);
        assert_eq!(error.exit_code(), INVALID_OPERAND_EXIT_CODE);
        assert!(matches!(
            error.kind(),
            LocalCopyErrorKind::InvalidArgument(_)
        ));
    }

    #[test]
    fn local_copy_error_delete_limit_exceeded() {
        let error = LocalCopyError::delete_limit_exceeded(100);
        assert_eq!(error.exit_code(), MAX_DELETE_EXIT_CODE);
        assert!(matches!(
            error.kind(),
            LocalCopyErrorKind::DeleteLimitExceeded { skipped: 100 }
        ));
    }

    #[test]
    fn local_copy_error_delete_limit_exceeded_message_singular() {
        let error = LocalCopyError::delete_limit_exceeded(1);
        let message = error.to_string();
        assert!(message.contains("1 entry skipped"));
    }

    #[test]
    fn local_copy_error_delete_limit_exceeded_message_plural() {
        let error = LocalCopyError::delete_limit_exceeded(5);
        let message = error.to_string();
        assert!(message.contains("5 entries skipped"));
    }

    #[test]
    fn local_copy_error_io() {
        let io_err = io::Error::new(ErrorKind::NotFound, "file not found");
        let error = LocalCopyError::io("read", PathBuf::from("/test/file.txt"), io_err);
        assert_eq!(error.exit_code(), INVALID_OPERAND_EXIT_CODE);
        assert!(matches!(error.kind(), LocalCopyErrorKind::Io { .. }));
    }

    #[test]
    fn local_copy_error_io_message() {
        let io_err = io::Error::new(ErrorKind::NotFound, "file not found");
        let error = LocalCopyError::io("read", PathBuf::from("/test/file.txt"), io_err);
        let message = error.to_string();
        assert!(message.contains("read"));
        assert!(message.contains("/test/file.txt"));
    }

    #[test]
    fn local_copy_error_timeout() {
        let error = LocalCopyError::timeout(Duration::from_secs(30));
        assert_eq!(error.exit_code(), TIMEOUT_EXIT_CODE);
        assert!(matches!(error.kind(), LocalCopyErrorKind::Timeout { .. }));
    }

    #[test]
    fn local_copy_error_timeout_message() {
        let error = LocalCopyError::timeout(Duration::from_secs(30));
        let message = error.to_string();
        assert!(message.contains("timed out"));
        assert!(message.contains("30"));
    }

    #[test]
    fn local_copy_error_stop_at_reached() {
        let target = SystemTime::now();
        let error = LocalCopyError::stop_at_reached(target);
        assert_eq!(error.exit_code(), TIMEOUT_EXIT_CODE);
        assert!(matches!(
            error.kind(),
            LocalCopyErrorKind::StopAtReached { .. }
        ));
    }

    #[test]
    fn local_copy_error_into_kind() {
        let error = LocalCopyError::missing_operands();
        let kind = error.into_kind();
        assert!(matches!(kind, LocalCopyErrorKind::MissingSourceOperands));
    }

    #[test]
    fn local_copy_error_kind_as_io() {
        let io_err = io::Error::new(ErrorKind::NotFound, "file not found");
        let error = LocalCopyError::io("read", PathBuf::from("/test/file.txt"), io_err);
        let (action, path, _source) = error.kind().as_io().expect("should be Io variant");
        assert_eq!(action, "read");
        assert_eq!(path, Path::new("/test/file.txt"));
    }

    #[test]
    fn local_copy_error_kind_as_io_returns_none_for_other_variants() {
        let error = LocalCopyError::missing_operands();
        assert!(error.kind().as_io().is_none());
    }

    #[test]
    fn local_copy_argument_error_empty_source_message() {
        let error = LocalCopyArgumentError::EmptySourceOperand;
        assert!(error.message().contains("source operands"));
    }

    #[test]
    fn local_copy_argument_error_empty_destination_message() {
        let error = LocalCopyArgumentError::EmptyDestinationOperand;
        assert!(error.message().contains("destination operand"));
    }

    #[test]
    fn local_copy_argument_error_destination_must_be_directory_message() {
        let error = LocalCopyArgumentError::DestinationMustBeDirectory;
        assert!(error.message().contains("existing directory"));
    }

    #[test]
    fn local_copy_argument_error_remote_operand_message() {
        let error = LocalCopyArgumentError::RemoteOperandUnsupported;
        assert!(error.message().contains("remote operands"));
        assert!(error.message().contains("OC_RSYNC_FALLBACK"));
    }

    #[test]
    fn local_copy_argument_error_all_variants_have_messages() {
        // Test that all variants have non-empty messages
        let variants = [
            LocalCopyArgumentError::EmptySourceOperand,
            LocalCopyArgumentError::EmptyDestinationOperand,
            LocalCopyArgumentError::DestinationMustBeDirectory,
            LocalCopyArgumentError::DirectoryNameUnavailable,
            LocalCopyArgumentError::FileNameUnavailable,
            LocalCopyArgumentError::LinkNameUnavailable,
            LocalCopyArgumentError::UnsupportedFileType,
            LocalCopyArgumentError::ReplaceDirectoryWithSymlink,
            LocalCopyArgumentError::ReplaceDirectoryWithFile,
            LocalCopyArgumentError::ReplaceDirectoryWithSpecial,
            LocalCopyArgumentError::ReplaceNonDirectoryWithDirectory,
            LocalCopyArgumentError::RemoteOperandUnsupported,
        ];

        for variant in variants {
            assert!(!variant.message().is_empty());
        }
    }
}
