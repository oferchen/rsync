use std::io;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use thiserror::Error;

use super::filter_program::{
    INVALID_OPERAND_EXIT_CODE, MAX_DELETE_EXIT_CODE, MISSING_OPERANDS_EXIT_CODE, TIMEOUT_EXIT_CODE,
    VANISHED_EXIT_CODE,
};

/// Formats an [`io::Error`] the way upstream rsync's `rsyserr()` does:
/// `"<strerror> (<errno>)"` (upstream `log.c:473` `": %s (%d)"`), rather than
/// Rust's `std::io::Error` `Display`, which renders `" (os error <errno>)"`.
/// Errors without an OS errno fall back to the `Display` string verbatim.
#[must_use]
pub fn upstream_io_error(error: &io::Error) -> String {
    match error.raw_os_error() {
        Some(code) => {
            let full = error.to_string();
            let strerror = full
                .strip_suffix(&format!(" (os error {code})"))
                .unwrap_or(full.as_str());
            format!("{strerror} ({code})")
        }
        None => error.to_string(),
    }
}

/// Error produced when planning or executing a local copy fails.
///
/// # Exit Code Integration
///
/// This error type uses exit codes that match upstream rsync's `errcode.h`.
/// When this error bubbles up to the `core` crate (via `ClientError`), the
/// exit codes correspond to `core::exit_code::ExitCode` variants:
///
/// | Exit Code | Upstream Name | core::exit_code::ExitCode |
/// |-----------|---------------|---------------------------|
/// | 1         | RERR_SYNTAX   | `Syntax`                  |
/// | 23        | RERR_PARTIAL  | `PartialTransfer`         |
/// | 24        | RERR_VANISHED | `Vanished`                |
/// | 25        | RERR_DEL_LIMIT| `DeleteLimit`             |
/// | 30        | RERR_TIMEOUT  | `Timeout`                 |
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
    ///
    /// Accepts any type that can be converted to `PathBuf`, including `&Path`,
    /// `PathBuf`, `&str`, and `String`.
    #[must_use]
    pub fn io(action: &'static str, path: impl Into<PathBuf>, source: io::Error) -> Self {
        Self::new(LocalCopyErrorKind::Io {
            action,
            path: path.into(),
            source,
        })
    }

    /// Constructs an error for a source argument whose initial `link_stat`
    /// failed because the path does not exist.
    ///
    /// Distinct from [`LocalCopyError::io`]'s NotFound mapping: a *source* (a
    /// command-line operand or a `--files-from` entry) that is missing when the
    /// sender stats it exits 23 (`RERR_PARTIAL`) with a `link_stat "%s" failed`
    /// message, and the transfer continues with the remaining sources. Exit 24
    /// (`RERR_VANISHED`, "file has vanished") is reserved for a file that
    /// disappears mid-transfer after it was already in the file list.
    ///
    /// upstream: `flist.c send_file_list()` - a failed `link_stat` on an arg
    /// sets `io_error |= IOERR_GENERAL`, yielding `exit_cleanup(RERR_PARTIAL)`.
    #[must_use]
    pub fn link_stat_failed(path: impl Into<PathBuf>, source: io::Error) -> Self {
        Self::new(LocalCopyErrorKind::LinkStatFailed {
            path: path.into(),
            source,
        })
    }

    /// Constructs a partial-transfer error (exit code 23, `RERR_PARTIAL`).
    ///
    /// Raised at the end of a copy that skipped one or more entries without a
    /// per-source error to propagate - for example an `--iconv` filename that
    /// could not be transcoded to the remote charset. The per-entry diagnostic
    /// is emitted at the skip site; this error only carries the exit code and
    /// the summary message.
    ///
    /// upstream: `main.c:1338-1345` `log_exit()` maps `io_error &
    /// IOERR_GENERAL` to `RERR_PARTIAL`.
    #[must_use]
    pub const fn partial_transfer() -> Self {
        Self::new(LocalCopyErrorKind::PartialTransfer)
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

    /// Constructs a filter syntax error (exit code 1, `RERR_SYNTAX`).
    ///
    /// upstream: exclude.c:1212 - unrecognised filter rules exit with
    /// `RERR_SYNTAX` (1), not `RERR_PARTIAL` (23).
    #[must_use]
    pub fn filter_syntax(message: impl Into<String>) -> Self {
        Self::new(LocalCopyErrorKind::FilterSyntax {
            message: message.into(),
        })
    }

    /// Returns the exit code that mirrors upstream rsync's behaviour.
    ///
    /// See the struct-level documentation for mappings to `core::exit_code::ExitCode`.
    ///
    /// For I/O errors, `NotFound` maps to exit code 24 (`RERR_VANISHED`) while
    /// all other I/O errors map to exit code 23 (`RERR_PARTIAL`).
    ///
    /// # Upstream Reference
    ///
    /// - `main.c:1338-1345`: `log_exit()` maps `io_error` flags to exit codes
    #[must_use]
    pub fn exit_code(&self) -> i32 {
        match &self.kind {
            LocalCopyErrorKind::MissingSourceOperands => MISSING_OPERANDS_EXIT_CODE,
            LocalCopyErrorKind::InvalidArgument(_) => INVALID_OPERAND_EXIT_CODE,
            LocalCopyErrorKind::Io { source, .. } => {
                if source.kind() == io::ErrorKind::NotFound {
                    VANISHED_EXIT_CODE
                } else {
                    INVALID_OPERAND_EXIT_CODE
                }
            }
            LocalCopyErrorKind::LinkStatFailed { .. } => INVALID_OPERAND_EXIT_CODE,
            LocalCopyErrorKind::Timeout { .. } | LocalCopyErrorKind::StopAtReached { .. } => {
                TIMEOUT_EXIT_CODE
            }
            LocalCopyErrorKind::DeleteLimitExceeded { .. } => MAX_DELETE_EXIT_CODE,
            LocalCopyErrorKind::FilterSyntax { .. } => MISSING_OPERANDS_EXIT_CODE,
            LocalCopyErrorKind::PartialTransfer => INVALID_OPERAND_EXIT_CODE,
        }
    }

    /// Returns the upstream rsync error code name for debugging.
    ///
    /// These names correspond to constants in upstream rsync's `errcode.h`.
    #[must_use]
    pub fn code_name(&self) -> &'static str {
        match &self.kind {
            LocalCopyErrorKind::MissingSourceOperands => "RERR_SYNTAX",
            LocalCopyErrorKind::InvalidArgument(_) => "RERR_PARTIAL",
            LocalCopyErrorKind::Io { source, .. } => {
                if source.kind() == io::ErrorKind::NotFound {
                    "RERR_VANISHED"
                } else {
                    "RERR_PARTIAL"
                }
            }
            LocalCopyErrorKind::LinkStatFailed { .. } => "RERR_PARTIAL",
            LocalCopyErrorKind::Timeout { .. } | LocalCopyErrorKind::StopAtReached { .. } => {
                "RERR_TIMEOUT"
            }
            LocalCopyErrorKind::DeleteLimitExceeded { .. } => "RERR_DEL_LIMIT",
            LocalCopyErrorKind::FilterSyntax { .. } => "RERR_SYNTAX",
            LocalCopyErrorKind::PartialTransfer => "RERR_PARTIAL",
        }
    }

    /// Reports whether this is an I/O error (filesystem interaction failure).
    #[must_use]
    pub const fn is_io_error(&self) -> bool {
        matches!(self.kind, LocalCopyErrorKind::Io { .. })
    }

    /// Reports whether this error represents a vanished file (NotFound I/O error).
    ///
    /// Upstream rsync gracefully skips files that vanish during the transfer
    /// regardless of whether `--delete` is active, logging a warning and
    /// continuing with the remaining entries.
    ///
    /// # Upstream Reference
    ///
    /// - `generator.c`: vanished files produce a warning, not a fatal error
    /// - Exit code 24 (`RERR_VANISHED`) when files disappear during transfer
    #[must_use]
    pub fn is_vanished_error(&self) -> bool {
        matches!(
            &self.kind,
            LocalCopyErrorKind::Io { source, .. }
                if source.kind() == io::ErrorKind::NotFound
        )
    }

    /// Reports whether this error is the `--max-delete` limit being reached.
    ///
    /// Upstream rsync does not abort the transfer when the limit is hit: it
    /// stops performing further deletions, finishes the transfer, and only
    /// reports the limit at cleanup (`main.c:1356`, exit code 25). The
    /// delete-during path defers this error to the end of the directory so a
    /// mid-transfer `--delete-during` sweep does not skip pending copies.
    #[must_use]
    pub const fn is_delete_limit_error(&self) -> bool {
        matches!(self.kind, LocalCopyErrorKind::DeleteLimitExceeded { .. })
    }

    /// Reports whether this is a failed initial `link_stat` of a source
    /// argument (a missing command-line operand or `--files-from` entry).
    ///
    /// Like a vanished file, the transfer continues with the remaining
    /// sources, but the exit code is 23 (`RERR_PARTIAL`), not 24.
    #[must_use]
    pub const fn is_link_stat_failed(&self) -> bool {
        matches!(self.kind, LocalCopyErrorKind::LinkStatFailed { .. })
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
    /// The initial `link_stat` of a source argument failed because the path
    /// does not exist. A hard error exiting 23 (`RERR_PARTIAL`), distinct from
    /// a mid-transfer vanish (exit 24); the caller continues with the remaining
    /// sources.
    #[error("link_stat \"{}\" failed: {}", path.display(), upstream_io_error(source))]
    LinkStatFailed {
        /// The source path that could not be stat'd.
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
    // upstream: generator.c:2431 - `Deletions stopped due to --max-delete
    // limit (%d skipped)` with no pluralized noun.
    #[error("Deletions stopped due to --max-delete limit ({skipped} skipped)")]
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
    /// A filter rule could not be parsed.
    ///
    /// upstream: exclude.c:1212 - `Unknown filter rule: \`%s'` exits with
    /// `RERR_SYNTAX` (1).
    #[error("{message}")]
    FilterSyntax {
        /// Human-readable error message (already formatted to match upstream).
        message: String,
    },
    /// One or more entries were skipped, so the transfer completes with
    /// `RERR_PARTIAL` (exit 23). The per-entry cause was already reported at
    /// the skip site (e.g. an unconvertible `--iconv` filename).
    ///
    /// upstream: `main.c:1356` prints `some files/attrs were not transferred
    /// (see previous errors)` when `io_error` is set at exit.
    #[error("some files/attrs were not transferred (see previous errors)")]
    PartialTransfer,
}

impl LocalCopyErrorKind {
    /// Returns the action, path, and source error for [`LocalCopyErrorKind::Io`] values.
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
    fn local_copy_error_delete_limit_exceeded_message_matches_upstream() {
        // upstream: generator.c:2431 emits `Deletions stopped due to
        // --max-delete limit (%d skipped)` verbatim with no `entry`/`entries`
        // noun, so the count renders identically for one or many skips.
        let one = LocalCopyError::delete_limit_exceeded(1).to_string();
        let many = LocalCopyError::delete_limit_exceeded(5).to_string();
        assert!(
            one.contains("Deletions stopped due to --max-delete limit (1 skipped)"),
            "{one}"
        );
        assert!(
            many.contains("Deletions stopped due to --max-delete limit (5 skipped)"),
            "{many}"
        );
        assert!(!one.contains("entry"), "{one}");
        assert!(!many.contains("entries"), "{many}");
    }

    #[test]
    fn local_copy_error_io() {
        let io_err = io::Error::new(ErrorKind::PermissionDenied, "permission denied");
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

    #[test]
    fn is_vanished_error_returns_true_for_not_found() {
        let io_err = io::Error::new(ErrorKind::NotFound, "file not found");
        let error = LocalCopyError::io("read", PathBuf::from("/vanished"), io_err);
        assert!(error.is_vanished_error());
    }

    #[test]
    fn is_vanished_error_returns_false_for_other_io_errors() {
        let io_err = io::Error::new(ErrorKind::PermissionDenied, "access denied");
        let error = LocalCopyError::io("read", PathBuf::from("/denied"), io_err);
        assert!(!error.is_vanished_error());
    }

    #[test]
    fn is_vanished_error_returns_false_for_non_io_errors() {
        let error = LocalCopyError::missing_operands();
        assert!(!error.is_vanished_error());
    }

    #[test]
    fn code_name_for_missing_operands() {
        let error = LocalCopyError::missing_operands();
        assert_eq!(error.code_name(), "RERR_SYNTAX");
    }

    #[test]
    fn code_name_for_invalid_argument() {
        let error = LocalCopyError::invalid_argument(LocalCopyArgumentError::EmptySourceOperand);
        assert_eq!(error.code_name(), "RERR_PARTIAL");
    }

    #[test]
    fn code_name_for_io_vanished() {
        let io_err = io::Error::new(ErrorKind::NotFound, "file not found");
        let error = LocalCopyError::io("read", PathBuf::from("/test"), io_err);
        assert_eq!(error.code_name(), "RERR_VANISHED");
    }

    #[test]
    fn code_name_for_io_general() {
        let io_err = io::Error::new(ErrorKind::PermissionDenied, "access denied");
        let error = LocalCopyError::io("read", PathBuf::from("/test"), io_err);
        assert_eq!(error.code_name(), "RERR_PARTIAL");
    }

    #[test]
    fn code_name_for_timeout() {
        let error = LocalCopyError::timeout(Duration::from_secs(30));
        assert_eq!(error.code_name(), "RERR_TIMEOUT");
    }

    #[test]
    fn code_name_for_delete_limit() {
        let error = LocalCopyError::delete_limit_exceeded(100);
        assert_eq!(error.code_name(), "RERR_DEL_LIMIT");
    }

    #[test]
    fn code_name_for_stop_at_reached() {
        let error = LocalCopyError::stop_at_reached(SystemTime::now());
        assert_eq!(error.code_name(), "RERR_TIMEOUT");
    }

    #[test]
    fn exit_code_vanished_returns_24() {
        let io_err = io::Error::new(ErrorKind::NotFound, "file vanished");
        let error = LocalCopyError::io("read", PathBuf::from("/gone"), io_err);
        assert_eq!(error.exit_code(), VANISHED_EXIT_CODE);
        assert_eq!(error.exit_code(), 24);
    }

    #[test]
    fn exit_code_permission_denied_returns_23() {
        let io_err = io::Error::new(ErrorKind::PermissionDenied, "access denied");
        let error = LocalCopyError::io("read", PathBuf::from("/denied"), io_err);
        assert_eq!(error.exit_code(), INVALID_OPERAND_EXIT_CODE);
        assert_eq!(error.exit_code(), 23);
    }

    #[test]
    fn link_stat_failed_is_partial_and_continues() {
        // A missing source argument (command-line operand or --files-from
        // entry) is a failed link_stat: exit 23 (RERR_PARTIAL), classified for
        // continue-with-remaining but NOT as a mid-transfer vanish (24).
        let io_err = io::Error::new(ErrorKind::NotFound, "no such file");
        let error = LocalCopyError::link_stat_failed(PathBuf::from("/tmp/nope"), io_err);
        assert_eq!(error.exit_code(), INVALID_OPERAND_EXIT_CODE);
        assert_eq!(error.exit_code(), 23);
        assert_eq!(error.code_name(), "RERR_PARTIAL");
        assert!(error.is_link_stat_failed());
        assert!(!error.is_vanished_error());
        assert!(!error.is_io_error());
        assert!(
            error
                .to_string()
                .starts_with("link_stat \"/tmp/nope\" failed")
        );
    }
}
