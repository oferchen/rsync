use crate::message::{Message, Role};
use crate::rsync_error;
use rsync_engine::local_copy::{LocalCopyError, LocalCopyErrorKind};
use std::error::Error;
use std::fmt;
use std::io;
use std::path::Path;

/// Exit code returned when client functionality is unavailable.
pub const FEATURE_UNAVAILABLE_EXIT_CODE: i32 = 1;
/// Exit code used when a copy partially or wholly fails.
pub const PARTIAL_TRANSFER_EXIT_CODE: i32 = 23;
/// Exit code returned when socket I/O fails.
pub const SOCKET_IO_EXIT_CODE: i32 = 10;
/// Exit code returned when the `--max-delete` limit stops deletions.
pub(crate) const MAX_DELETE_EXIT_CODE: i32 = 25;
/// Exit code returned when a daemon violates the protocol.
pub(crate) const PROTOCOL_INCOMPATIBLE_EXIT_CODE: i32 = 2;

/// Error returned when the client orchestration fails.
#[derive(Clone, Debug)]
pub struct ClientError {
    exit_code: i32,
    message: Message,
}

impl ClientError {
    /// Creates a new [`ClientError`] from the supplied message.
    pub(crate) fn new(exit_code: i32, message: Message) -> Self {
        Self { exit_code, message }
    }

    /// Returns the exit code associated with this error.
    #[must_use]
    pub const fn exit_code(&self) -> i32 {
        self.exit_code
    }

    /// Returns the formatted diagnostic message that should be emitted.
    pub fn message(&self) -> &Message {
        &self.message
    }
}

impl fmt::Display for ClientError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.message.fmt(f)
    }
}

impl Error for ClientError {}

pub(crate) fn missing_operands_error() -> ClientError {
    let message = rsync_error!(
        FEATURE_UNAVAILABLE_EXIT_CODE,
        "missing source operands: supply at least one source and a destination"
    )
    .with_role(Role::Client);
    ClientError::new(FEATURE_UNAVAILABLE_EXIT_CODE, message)
}

pub(crate) fn invalid_argument_error(text: &str, exit_code: i32) -> ClientError {
    let message = rsync_error!(exit_code, "{}", text).with_role(Role::Client);
    ClientError::new(exit_code, message)
}

pub(crate) fn map_local_copy_error(error: LocalCopyError) -> ClientError {
    let exit_code = error.exit_code();
    match error.into_kind() {
        LocalCopyErrorKind::MissingSourceOperands => missing_operands_error(),
        LocalCopyErrorKind::InvalidArgument(reason) => {
            invalid_argument_error(reason.message(), exit_code)
        }
        LocalCopyErrorKind::Io {
            action,
            path,
            source,
        } => io_error(action, &path, source),
        LocalCopyErrorKind::Timeout { duration } => {
            let text = format!(
                "transfer timed out after {:.3} seconds without progress",
                duration.as_secs_f64()
            );
            let message = rsync_error!(exit_code, text).with_role(Role::Client);
            ClientError::new(exit_code, message)
        }
        LocalCopyErrorKind::DeleteLimitExceeded { skipped } => {
            let noun = if skipped == 1 { "entry" } else { "entries" };
            let text =
                format!("Deletions stopped due to --max-delete limit ({skipped} {noun} skipped)");
            let message = rsync_error!(MAX_DELETE_EXIT_CODE, text).with_role(Role::Client);
            ClientError::new(MAX_DELETE_EXIT_CODE, message)
        }
        LocalCopyErrorKind::StopAtReached { .. } => {
            let message =
                rsync_error!(exit_code, "stopping at requested limit").with_role(Role::Client);
            ClientError::new(exit_code, message)
        }
    }
}

pub(crate) fn compile_filter_error(pattern: &str, error: &dyn fmt::Display) -> ClientError {
    let text = format!("failed to compile filter pattern '{pattern}': {error}");
    let message = rsync_error!(FEATURE_UNAVAILABLE_EXIT_CODE, text).with_role(Role::Client);
    ClientError::new(FEATURE_UNAVAILABLE_EXIT_CODE, message)
}

pub(crate) fn io_error(action: &str, path: &Path, error: io::Error) -> ClientError {
    let path_display = path.display();
    let text = format!("failed to {action} '{path_display}': {error}");
    let message = rsync_error!(PARTIAL_TRANSFER_EXIT_CODE, text).with_role(Role::Client);
    ClientError::new(PARTIAL_TRANSFER_EXIT_CODE, message)
}

pub(crate) fn socket_error(
    action: &str,
    target: impl fmt::Display,
    error: io::Error,
) -> ClientError {
    let text = format!("failed to {action} {target}: {error}");
    let message = rsync_error!(SOCKET_IO_EXIT_CODE, text).with_role(Role::Client);
    ClientError::new(SOCKET_IO_EXIT_CODE, message)
}

pub(crate) fn daemon_error(text: impl Into<String>, exit_code: i32) -> ClientError {
    let message = rsync_error!(exit_code, "{}", text.into()).with_role(Role::Client);
    ClientError::new(exit_code, message)
}

pub(crate) fn daemon_protocol_error(text: &str) -> ClientError {
    daemon_error(
        format!("unexpected response from daemon: {text}"),
        PROTOCOL_INCOMPATIBLE_EXIT_CODE,
    )
}

pub(crate) fn daemon_authentication_required_error(reason: &str) -> ClientError {
    let detail = if reason.is_empty() {
        "daemon requires authentication for module listing".to_string()
    } else {
        format!("daemon requires authentication for module listing: {reason}")
    };

    daemon_error(detail, FEATURE_UNAVAILABLE_EXIT_CODE)
}

pub(crate) fn daemon_authentication_failed_error(reason: Option<&str>) -> ClientError {
    let detail = match reason {
        Some(text) if !text.is_empty() => {
            format!("daemon rejected provided credentials: {text}")
        }
        _ => "daemon rejected provided credentials".to_string(),
    };

    daemon_error(detail, FEATURE_UNAVAILABLE_EXIT_CODE)
}

pub(crate) fn daemon_access_denied_error(reason: &str) -> ClientError {
    let detail = if reason.is_empty() {
        "daemon denied access to module listing".to_string()
    } else {
        format!("daemon denied access to module listing: {reason}")
    };

    daemon_error(detail, PARTIAL_TRANSFER_EXIT_CODE)
}

pub(crate) fn daemon_listing_unavailable_error(reason: &str) -> ClientError {
    let trimmed = reason.trim();
    let detail = if trimmed.is_empty() {
        "daemon refused module listing".to_string()
    } else {
        format!("daemon refused module listing: {trimmed}")
    };

    daemon_error(detail, FEATURE_UNAVAILABLE_EXIT_CODE)
}
