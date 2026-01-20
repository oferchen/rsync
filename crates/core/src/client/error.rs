use std::fmt;
use std::io;
use std::path::Path;

use thiserror::Error;

use crate::exit_code::{ExitCode, HasExitCode};
use crate::message::{Message, Role};
use crate::rsync_error;
use engine::local_copy::{LocalCopyError, LocalCopyErrorKind};

// Re-export exit code constants for backward compatibility.
// These map directly to ExitCode variants and should be preferred
// when type safety is important.

/// Exit code returned when client functionality is unavailable.
pub const FEATURE_UNAVAILABLE_EXIT_CODE: i32 = ExitCode::Syntax.as_i32();
/// Exit code returned when a daemon violates the protocol.
pub const PROTOCOL_INCOMPATIBLE_EXIT_CODE: i32 = ExitCode::Protocol.as_i32();
/// Exit code returned for errors selecting input/output files or directories.
pub const FILE_SELECTION_EXIT_CODE: i32 = ExitCode::FileSelect.as_i32();
/// Exit code returned when starting client-server protocol fails.
pub const CLIENT_SERVER_PROTOCOL_EXIT_CODE: i32 = ExitCode::StartClient.as_i32();
/// Exit code returned when socket I/O fails.
pub const SOCKET_IO_EXIT_CODE: i32 = ExitCode::SocketIo.as_i32();
/// Exit code returned when file I/O fails.
pub const FILE_IO_EXIT_CODE: i32 = ExitCode::FileIo.as_i32();
/// Exit code returned for IPC errors (inter-process communication).
pub const IPC_EXIT_CODE: i32 = ExitCode::Ipc.as_i32();
/// Exit code used when a copy partially or wholly fails.
pub const PARTIAL_TRANSFER_EXIT_CODE: i32 = ExitCode::PartialTransfer.as_i32();
/// Exit code returned when remote command is not found.
pub const REMOTE_COMMAND_NOT_FOUND_EXIT_CODE: i32 = ExitCode::CommandNotFound.as_i32();

/// Error returned when the client orchestration fails.
///
/// Uses the centralized [`ExitCode`] enum to ensure exit codes match
/// upstream rsync behavior.
#[derive(Clone, Debug, Error)]
#[error("{message}")]
pub struct ClientError {
    exit_code: ExitCode,
    message: Message,
}

impl ClientError {
    /// Creates a new [`ClientError`] from the supplied exit code and message.
    pub(crate) const fn with_code(exit_code: ExitCode, message: Message) -> Self {
        Self { exit_code, message }
    }

    /// Creates a new [`ClientError`] from an i32 exit code and message.
    ///
    /// If the exit code doesn't map to a known [`ExitCode`] variant,
    /// [`ExitCode::PartialTransfer`] is used as a fallback.
    ///
    /// This is the primary constructor for backward compatibility with
    /// existing code that uses i32 exit codes.
    pub(crate) fn new(exit_code: i32, message: Message) -> Self {
        let code = ExitCode::from_i32(exit_code).unwrap_or(ExitCode::PartialTransfer);
        Self::with_code(code, message)
    }

    /// Returns the exit code associated with this error.
    #[must_use]
    pub const fn code(&self) -> ExitCode {
        self.exit_code
    }

    /// Returns the exit code as an i32 for backward compatibility.
    #[must_use]
    pub const fn exit_code(&self) -> i32 {
        self.exit_code.as_i32()
    }

    /// Returns the formatted diagnostic message that should be emitted.
    pub const fn message(&self) -> &Message {
        &self.message
    }
}

impl HasExitCode for ClientError {
    fn exit_code(&self) -> ExitCode {
        self.exit_code
    }
}

#[cold]
pub(crate) fn missing_operands_error() -> ClientError {
    let code = ExitCode::PartialTransfer;
    let message = rsync_error!(
        code.as_i32(),
        "missing source operands: supply at least one source and a destination"
    )
    .with_role(Role::Client);
    // Mirror upstream: return exit code 23 for missing source operands
    ClientError::with_code(code, message)
}

#[cold]
#[allow(dead_code)]
pub(crate) fn fallback_disabled_error() -> ClientError {
    let code = ExitCode::Syntax;
    let message = rsync_error!(
        code.as_i32(),
        "remote transfers require native support; fallback to system rsync is disabled"
    )
    .with_role(Role::Client);
    ClientError::with_code(code, message)
}

/// Creates an invalid argument error from an i32 exit code.
///
/// If the exit code doesn't map to a known [`ExitCode`] variant,
/// [`ExitCode::PartialTransfer`] is used as a fallback.
#[cold]
pub(crate) fn invalid_argument_error(text: &str, exit_code: i32) -> ClientError {
    let code = ExitCode::from_i32(exit_code).unwrap_or(ExitCode::PartialTransfer);
    let message = rsync_error!(code.as_i32(), "{}", text).with_role(Role::Client);
    ClientError::with_code(code, message)
}

/// Creates an invalid argument error with a typed exit code.
#[cold]
#[allow(dead_code)]
pub(crate) fn invalid_argument_error_typed(text: &str, exit_code: ExitCode) -> ClientError {
    let message = rsync_error!(exit_code.as_i32(), "{}", text).with_role(Role::Client);
    ClientError::with_code(exit_code, message)
}

#[cold]
pub(crate) fn map_local_copy_error(error: LocalCopyError) -> ClientError {
    let exit_code_i32 = error.exit_code();
    match error.into_kind() {
        LocalCopyErrorKind::MissingSourceOperands => missing_operands_error(),
        LocalCopyErrorKind::InvalidArgument(reason) => {
            invalid_argument_error(reason.message(), exit_code_i32)
        }
        LocalCopyErrorKind::Io {
            action,
            path,
            source,
        } => io_error(action, &path, source),
        LocalCopyErrorKind::Timeout { duration } => {
            let code = ExitCode::Timeout;
            let text = format!(
                "transfer timed out after {:.3} seconds without progress",
                duration.as_secs_f64()
            );
            let message = rsync_error!(code.as_i32(), text).with_role(Role::Client);
            ClientError::with_code(code, message)
        }
        LocalCopyErrorKind::DeleteLimitExceeded { skipped } => {
            let code = ExitCode::DeleteLimit;
            let noun = if skipped == 1 { "entry" } else { "entries" };
            let text =
                format!("Deletions stopped due to --max-delete limit ({skipped} {noun} skipped)");
            let message = rsync_error!(code.as_i32(), text).with_role(Role::Client);
            ClientError::with_code(code, message)
        }
        LocalCopyErrorKind::StopAtReached { .. } => {
            let code = ExitCode::from_i32(exit_code_i32).unwrap_or(ExitCode::PartialTransfer);
            let message =
                rsync_error!(code.as_i32(), "stopping at requested limit").with_role(Role::Client);
            ClientError::with_code(code, message)
        }
    }
}

#[cold]
pub(crate) fn compile_filter_error(pattern: &str, error: &dyn fmt::Display) -> ClientError {
    let code = ExitCode::Syntax;
    let text = format!("failed to compile filter pattern '{pattern}': {error}");
    let message = rsync_error!(code.as_i32(), text).with_role(Role::Client);
    ClientError::with_code(code, message)
}

#[cold]
pub(crate) fn io_error(action: &str, path: &Path, error: io::Error) -> ClientError {
    let code = ExitCode::PartialTransfer;
    let path_display = path.display();
    let text = format!("failed to {action} '{path_display}': {error}");
    // Mirror upstream: use PartialTransfer (23) for file I/O errors
    // Upstream uses exit code 23 broadly for any transfer errors
    let message = rsync_error!(code.as_i32(), text).with_role(Role::Client);
    ClientError::with_code(code, message)
}

#[cold]
pub(crate) fn destination_access_error(path: &Path, error: io::Error) -> ClientError {
    let code = ExitCode::FileSelect;
    let path_display = path.display();
    let text = format!("failed to access destination directory '{path_display}': {error}");
    // Mirror upstream: destination directory access errors return FileSelect (3)
    // This matches upstream main.c:751 change_dir validation
    let message = rsync_error!(code.as_i32(), text).with_role(Role::Client);
    ClientError::with_code(code, message)
}

#[cold]
pub(crate) fn socket_error(
    action: &str,
    target: impl fmt::Display,
    error: io::Error,
) -> ClientError {
    let code = ExitCode::SocketIo;
    let text = format!("failed to {action} {target}: {error}");
    let message = rsync_error!(code.as_i32(), text).with_role(Role::Client);
    ClientError::with_code(code, message)
}

/// Creates a daemon error from an i32 exit code.
///
/// If the exit code doesn't map to a known [`ExitCode`] variant,
/// [`ExitCode::PartialTransfer`] is used as a fallback.
#[cold]
pub(crate) fn daemon_error(text: impl Into<String>, exit_code: i32) -> ClientError {
    let code = ExitCode::from_i32(exit_code).unwrap_or(ExitCode::PartialTransfer);
    let message = rsync_error!(code.as_i32(), "{}", text.into()).with_role(Role::Client);
    ClientError::with_code(code, message)
}

/// Creates a daemon error with a typed exit code.
#[cold]
#[allow(dead_code)]
pub(crate) fn daemon_error_typed(text: impl Into<String>, exit_code: ExitCode) -> ClientError {
    let message = rsync_error!(exit_code.as_i32(), "{}", text.into()).with_role(Role::Client);
    ClientError::with_code(exit_code, message)
}

#[cold]
pub(crate) fn daemon_protocol_error(text: &str) -> ClientError {
    daemon_error(
        format!("unexpected response from daemon: {text}"),
        PROTOCOL_INCOMPATIBLE_EXIT_CODE,
    )
}

#[cold]
pub(crate) fn daemon_authentication_required_error(reason: &str) -> ClientError {
    let detail = if reason.is_empty() {
        "daemon requires authentication for module listing".to_owned()
    } else {
        format!("daemon requires authentication for module listing: {reason}")
    };

    daemon_error(detail, FEATURE_UNAVAILABLE_EXIT_CODE)
}

#[cold]
pub(crate) fn daemon_authentication_failed_error(reason: Option<&str>) -> ClientError {
    let detail = match reason {
        Some(text) if !text.is_empty() => {
            format!("daemon rejected provided credentials: {text}")
        }
        _ => "daemon rejected provided credentials".to_owned(),
    };

    daemon_error(detail, FEATURE_UNAVAILABLE_EXIT_CODE)
}

#[cold]
pub(crate) fn daemon_access_denied_error(reason: &str) -> ClientError {
    let detail = if reason.is_empty() {
        "daemon denied access to module listing".to_owned()
    } else {
        format!("daemon denied access to module listing: {reason}")
    };

    daemon_error(detail, PARTIAL_TRANSFER_EXIT_CODE)
}

#[cold]
pub(crate) fn daemon_listing_unavailable_error(reason: &str) -> ClientError {
    let trimmed = reason.trim();
    let detail = if trimmed.is_empty() {
        "daemon refused module listing".to_owned()
    } else {
        format!("daemon refused module listing: {trimmed}")
    };

    daemon_error(detail, FEATURE_UNAVAILABLE_EXIT_CODE)
}

/// Enables idiomatic error conversion using the `?` operator.
///
/// # Examples
///
/// ```ignore
/// fn do_local_copy() -> Result<(), ClientError> {
///     let result: Result<(), LocalCopyError> = local_copy_plan.execute();
///     result?; // Automatically converts LocalCopyError to ClientError
///     Ok(())
/// }
/// ```
impl From<LocalCopyError> for ClientError {
    fn from(error: LocalCopyError) -> Self {
        map_local_copy_error(error)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::ErrorKind;

    mod exit_codes_tests {
        use super::*;

        #[test]
        fn exit_codes_have_expected_values() {
            assert_eq!(FEATURE_UNAVAILABLE_EXIT_CODE, 1);
            assert_eq!(PROTOCOL_INCOMPATIBLE_EXIT_CODE, 2);
            assert_eq!(FILE_SELECTION_EXIT_CODE, 3);
            assert_eq!(CLIENT_SERVER_PROTOCOL_EXIT_CODE, 5);
            assert_eq!(SOCKET_IO_EXIT_CODE, 10);
            assert_eq!(FILE_IO_EXIT_CODE, 11);
            assert_eq!(IPC_EXIT_CODE, 14);
            assert_eq!(PARTIAL_TRANSFER_EXIT_CODE, 23);
            assert_eq!(ExitCode::DeleteLimit.as_i32(), 25);
            assert_eq!(REMOTE_COMMAND_NOT_FOUND_EXIT_CODE, 127);
        }
    }

    mod client_error_tests {
        use super::*;

        #[test]
        fn new_and_accessors() {
            let code = ExitCode::StartClient;
            let message = rsync_error!(code.as_i32(), "test error").with_role(Role::Client);
            let error = ClientError::with_code(code, message);

            assert_eq!(error.exit_code(), code.as_i32());
            assert_eq!(error.code(), code);
            // Verify message is accessible
            let _ = error.message();
        }

        #[test]
        fn clone() {
            let code = ExitCode::SocketIo;
            let message = rsync_error!(code.as_i32(), "socket error").with_role(Role::Client);
            let error = ClientError::with_code(code, message);
            let cloned = error.clone();

            assert_eq!(error.exit_code(), cloned.exit_code());
            assert_eq!(error.code(), cloned.code());
        }

        #[test]
        fn debug_format() {
            let code = ExitCode::Syntax;
            let message = rsync_error!(code.as_i32(), "debug test").with_role(Role::Client);
            let error = ClientError::with_code(code, message);
            let debug = format!("{error:?}");

            assert!(debug.contains("ClientError"));
            assert!(debug.contains("exit_code"));
        }

        #[test]
        fn display_format() {
            let code = ExitCode::Syntax;
            let message = rsync_error!(code.as_i32(), "display test message").with_role(Role::Client);
            let error = ClientError::with_code(code, message);
            let display = format!("{error}");

            assert!(display.contains("display test message"));
        }

        #[test]
        fn new_uses_fallback_for_unknown_code() {
            let message = rsync_error!(999, "unknown code").with_role(Role::Client);
            let error = ClientError::new(999, message);

            // Unknown exit codes fall back to PartialTransfer
            assert_eq!(error.code(), ExitCode::PartialTransfer);
        }

        #[test]
        fn has_exit_code_trait() {
            let code = ExitCode::Protocol;
            let message = rsync_error!(code.as_i32(), "test").with_role(Role::Client);
            let error = ClientError::with_code(code, message);

            // Test the HasExitCode trait
            let trait_code: ExitCode = HasExitCode::exit_code(&error);
            assert_eq!(trait_code, code);
        }

        #[test]
        fn from_local_copy_error() {
            let local_error = LocalCopyError::missing_operands();
            let client_error: ClientError = local_error.into();

            // Note: missing_operands maps to PartialTransfer (23) in ClientError
            // to match upstream rsync behavior, even though LocalCopyError uses Syntax (1)
            assert_eq!(client_error.code(), ExitCode::PartialTransfer);
        }

        #[test]
        fn from_local_copy_error_timeout() {
            use std::time::Duration;

            let local_error = LocalCopyError::timeout(Duration::from_secs(30));
            let client_error: ClientError = local_error.into();

            assert_eq!(client_error.code(), ExitCode::Timeout);
            assert!(client_error.to_string().contains("timed out"));
        }

        #[test]
        fn from_local_copy_error_delete_limit() {
            let local_error = LocalCopyError::delete_limit_exceeded(5);
            let client_error: ClientError = local_error.into();

            assert_eq!(client_error.code(), ExitCode::DeleteLimit);
            assert!(client_error.to_string().contains("max-delete"));
        }
    }

    mod error_factory_tests {
        use super::*;

        #[test]
        fn missing_operands_error_uses_correct_code() {
            let error = missing_operands_error();
            assert_eq!(error.exit_code(), PARTIAL_TRANSFER_EXIT_CODE);
            assert!(error.to_string().contains("missing source operands"));
        }

        #[test]
        fn fallback_disabled_error_uses_correct_code() {
            let error = fallback_disabled_error();
            assert_eq!(error.exit_code(), FEATURE_UNAVAILABLE_EXIT_CODE);
            assert!(error.to_string().contains("fallback"));
        }

        #[test]
        fn invalid_argument_error_uses_provided_code() {
            let error = invalid_argument_error("invalid option", FILE_SELECTION_EXIT_CODE);
            assert_eq!(error.exit_code(), FILE_SELECTION_EXIT_CODE);
            assert_eq!(error.code(), ExitCode::FileSelect);
            assert!(error.to_string().contains("invalid option"));
        }

        #[test]
        fn invalid_argument_error_uses_fallback_for_unknown() {
            let error = invalid_argument_error("unknown code", 999);
            assert_eq!(error.code(), ExitCode::PartialTransfer);
            assert!(error.to_string().contains("unknown code"));
        }

        #[test]
        fn invalid_argument_error_typed_uses_exit_code() {
            let error = invalid_argument_error_typed("typed error", ExitCode::FileSelect);
            assert_eq!(error.code(), ExitCode::FileSelect);
            assert!(error.to_string().contains("typed error"));
        }

        #[test]
        fn compile_filter_error_uses_correct_code() {
            let pattern = "*.txt";
            let parse_error = "invalid regex";
            let error = compile_filter_error(pattern, &parse_error);

            assert_eq!(error.exit_code(), FEATURE_UNAVAILABLE_EXIT_CODE);
            let msg = error.to_string();
            assert!(msg.contains("failed to compile filter pattern"));
            assert!(msg.contains(pattern));
            assert!(msg.contains(parse_error));
        }

        #[test]
        fn io_error_uses_correct_code() {
            let io_err = io::Error::new(ErrorKind::NotFound, "file not found");
            let error = io_error("read", Path::new("/test/file.txt"), io_err);

            assert_eq!(error.exit_code(), PARTIAL_TRANSFER_EXIT_CODE);
            let msg = error.to_string();
            assert!(msg.contains("failed to read"));
            assert!(msg.contains("/test/file.txt"));
        }

        #[test]
        fn destination_access_error_uses_correct_code() {
            let io_err = io::Error::new(ErrorKind::PermissionDenied, "access denied");
            let error = destination_access_error(Path::new("/var/dest"), io_err);

            assert_eq!(error.exit_code(), FILE_SELECTION_EXIT_CODE);
            let msg = error.to_string();
            assert!(msg.contains("failed to access destination directory"));
            assert!(msg.contains("/var/dest"));
        }

        #[test]
        fn socket_error_uses_correct_code() {
            let io_err = io::Error::new(ErrorKind::ConnectionRefused, "connection refused");
            let error = socket_error("connect to", "localhost:873", io_err);

            assert_eq!(error.exit_code(), SOCKET_IO_EXIT_CODE);
            let msg = error.to_string();
            assert!(msg.contains("failed to connect to localhost:873"));
        }

        #[test]
        fn daemon_error_uses_provided_code() {
            let code = ExitCode::Crashed.as_i32();
            let error = daemon_error("test daemon error", code);
            assert_eq!(error.exit_code(), code);
            assert_eq!(error.code(), ExitCode::Crashed);
            assert!(error.to_string().contains("test daemon error"));
        }

        #[test]
        fn daemon_error_uses_fallback_for_unknown() {
            let error = daemon_error("unknown daemon error", 999);
            assert_eq!(error.code(), ExitCode::PartialTransfer);
            assert!(error.to_string().contains("unknown daemon error"));
        }

        #[test]
        fn daemon_error_typed_uses_exit_code() {
            let error = daemon_error_typed("typed daemon error", ExitCode::Protocol);
            assert_eq!(error.code(), ExitCode::Protocol);
            assert!(error.to_string().contains("typed daemon error"));
        }

        #[test]
        fn daemon_protocol_error_uses_correct_code() {
            let error = daemon_protocol_error("malformed response");

            assert_eq!(error.exit_code(), PROTOCOL_INCOMPATIBLE_EXIT_CODE);
            let msg = error.to_string();
            assert!(msg.contains("unexpected response from daemon"));
            assert!(msg.contains("malformed response"));
        }

        #[test]
        fn daemon_authentication_required_error_with_empty_reason() {
            let error = daemon_authentication_required_error("");

            assert_eq!(error.exit_code(), FEATURE_UNAVAILABLE_EXIT_CODE);
            let msg = error.to_string();
            assert!(msg.contains("daemon requires authentication for module listing"));
            // When reason is empty, the message should not have a reason suffix
            assert!(!msg.contains("module listing: "));
        }

        #[test]
        fn daemon_authentication_required_error_with_reason() {
            let error = daemon_authentication_required_error("password required");

            assert_eq!(error.exit_code(), FEATURE_UNAVAILABLE_EXIT_CODE);
            let msg = error.to_string();
            assert!(msg.contains("module listing: password required"));
        }

        #[test]
        fn daemon_authentication_failed_error_with_none() {
            let error = daemon_authentication_failed_error(None);

            assert_eq!(error.exit_code(), FEATURE_UNAVAILABLE_EXIT_CODE);
            let msg = error.to_string();
            assert!(msg.contains("daemon rejected provided credentials"));
            // When reason is None, message should not have a reason suffix
            assert!(!msg.contains("credentials: "));
        }

        #[test]
        fn daemon_authentication_failed_error_with_empty_string() {
            let error = daemon_authentication_failed_error(Some(""));

            assert_eq!(error.exit_code(), FEATURE_UNAVAILABLE_EXIT_CODE);
            let msg = error.to_string();
            assert!(msg.contains("daemon rejected provided credentials"));
            // When reason is empty, message should not have a reason suffix
            assert!(!msg.contains("credentials: "));
        }

        #[test]
        fn daemon_authentication_failed_error_with_reason() {
            let error = daemon_authentication_failed_error(Some("wrong password"));

            assert_eq!(error.exit_code(), FEATURE_UNAVAILABLE_EXIT_CODE);
            let msg = error.to_string();
            assert!(msg.contains("credentials: wrong password"));
        }

        #[test]
        fn daemon_access_denied_error_with_empty_reason() {
            let error = daemon_access_denied_error("");

            assert_eq!(error.exit_code(), PARTIAL_TRANSFER_EXIT_CODE);
            let msg = error.to_string();
            assert!(msg.contains("daemon denied access to module listing"));
            // When reason is empty, message should not have a reason suffix
            assert!(!msg.contains("listing: "));
        }

        #[test]
        fn daemon_access_denied_error_with_reason() {
            let error = daemon_access_denied_error("IP not allowed");

            assert_eq!(error.exit_code(), PARTIAL_TRANSFER_EXIT_CODE);
            let msg = error.to_string();
            assert!(msg.contains("listing: IP not allowed"));
        }

        #[test]
        fn daemon_listing_unavailable_error_with_empty_reason() {
            let error = daemon_listing_unavailable_error("");

            assert_eq!(error.exit_code(), FEATURE_UNAVAILABLE_EXIT_CODE);
            let msg = error.to_string();
            assert!(msg.contains("daemon refused module listing"));
            // When reason is empty, message should not have a reason suffix
            assert!(!msg.contains("listing: "));
        }

        #[test]
        fn daemon_listing_unavailable_error_with_whitespace_reason() {
            let error = daemon_listing_unavailable_error("   ");

            assert_eq!(error.exit_code(), FEATURE_UNAVAILABLE_EXIT_CODE);
            let msg = error.to_string();
            assert!(msg.contains("daemon refused module listing"));
            // When reason is whitespace only, message should not have a reason suffix
            assert!(!msg.contains("listing: "));
        }

        #[test]
        fn daemon_listing_unavailable_error_with_reason() {
            let error = daemon_listing_unavailable_error("listing disabled");

            assert_eq!(error.exit_code(), FEATURE_UNAVAILABLE_EXIT_CODE);
            let msg = error.to_string();
            assert!(msg.contains("listing: listing disabled"));
        }
    }
}
