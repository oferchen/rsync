//! Centralized exit code definitions matching upstream rsync.
//!
//! This module provides a unified [`ExitCode`](crate::exit_code::ExitCode) enum that mirrors the exit codes
//! defined in upstream rsync's `errcode.h`. All error types across the workspace
//! should use these codes to ensure consistent behavior with upstream rsync.
//!
//! # Upstream Reference
//!
//! Exit codes are defined in `errcode.h` and their string mappings are in `log.c`.
//! This implementation maintains exact compatibility with rsync 3.4.1.
//!
//! # Examples
//!
//! ```ignore
//! // Note: Example uses `ignore` because the crate name "core" conflicts
//! // with Rust's standard library `core` crate in doctest contexts.
//! use core::exit_code::ExitCode;
//!
//! let code = ExitCode::PartialTransfer;
//! assert_eq!(code.as_i32(), 23);
//! assert_eq!(code.description(), "partial transfer");
//! ```

use std::fmt;

/// Exit codes returned by rsync operations.
///
/// These codes match upstream rsync's `errcode.h` exactly. Each variant
/// includes documentation explaining when it should be used.
///
/// # Upstream Reference
///
/// Source: `errcode.h` in rsync 3.4.1
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[repr(i32)]
pub enum ExitCode {
    /// Successful completion (RERR_OK = 0).
    Ok = 0,

    /// Syntax or usage error (RERR_SYNTAX = 1).
    ///
    /// Returned when command-line arguments are invalid or a feature
    /// is unavailable.
    Syntax = 1,

    /// Protocol incompatibility (RERR_PROTOCOL = 2).
    ///
    /// Returned when the client and server cannot agree on a protocol
    /// version or when the protocol is violated.
    Protocol = 2,

    /// Errors selecting input/output files or directories (RERR_FILESELECT = 3).
    ///
    /// Returned when the specified source or destination cannot be accessed.
    FileSelect = 3,

    /// Requested action not supported (RERR_UNSUPPORTED = 4).
    ///
    /// Returned when attempting to use a feature not compiled in or
    /// not supported by the remote rsync.
    Unsupported = 4,

    /// Error starting client-server protocol (RERR_STARTCLIENT = 5).
    ///
    /// Returned when the initial handshake with the daemon fails.
    StartClient = 5,

    /// Daemon unable to append to log-file (RERR_LOG_FAILURE = 6).
    ///
    /// Returned when the daemon cannot write to its log file.
    LogFileAppend = 6,

    /// Error in socket I/O (RERR_SOCKETIO = 10).
    ///
    /// Returned for network-level errors during transfer.
    SocketIo = 10,

    /// Error in file I/O (RERR_FILEIO = 11).
    ///
    /// Returned for local filesystem errors during transfer.
    FileIo = 11,

    /// Error in rsync protocol data stream (RERR_STREAMIO = 12).
    ///
    /// Returned when the multiplexed data stream is corrupted.
    StreamIo = 12,

    /// Errors with program diagnostics (RERR_MESSAGEIO = 13).
    ///
    /// Returned when diagnostic message handling fails.
    MessageIo = 13,

    /// Error in IPC code (RERR_IPC = 14).
    ///
    /// Returned for inter-process communication failures.
    Ipc = 14,

    /// Sibling process crashed (RERR_CRASHED = 15).
    ///
    /// Returned when a child process terminates abnormally.
    Crashed = 15,

    /// Sibling terminated abnormally (RERR_TERMINATED = 16).
    ///
    /// Returned when a child process is killed by a signal.
    Terminated = 16,

    /// Status returned when sent SIGUSR1 (RERR_SIGNAL1 = 19).
    Signal1 = 19,

    /// Status returned when sent SIGINT, SIGTERM, SIGHUP (RERR_SIGNAL = 20).
    Signal = 20,

    /// Error returned by waitpid() (RERR_WAITCHILD = 21).
    WaitChild = 21,

    /// Error allocating core memory buffers (RERR_MALLOC = 22).
    Malloc = 22,

    /// Partial transfer due to error (RERR_PARTIAL = 23).
    ///
    /// The most common error code, returned when some files could not
    /// be transferred due to I/O errors or other issues.
    PartialTransfer = 23,

    /// File(s) vanished on sender side (RERR_VANISHED = 24).
    ///
    /// Returned when files disappear between file list generation and transfer.
    Vanished = 24,

    /// Skipped some deletes due to --max-delete (RERR_DEL_LIMIT = 25).
    ///
    /// Returned when the deletion limit prevented some deletions.
    DeleteLimit = 25,

    /// Timeout in data send/receive (RERR_TIMEOUT = 30).
    ///
    /// Returned when a transfer times out due to inactivity.
    Timeout = 30,

    /// Timeout waiting for daemon connection (RERR_CONTIMEOUT = 35).
    ///
    /// Returned when connecting to a daemon times out.
    ConnectionTimeout = 35,

    /// Command exited with status 255 (RERR_CMD_FAILED = 124).
    CommandFailed = 124,

    /// Command killed by signal (RERR_CMD_KILLED = 125).
    CommandKilled = 125,

    /// Command cannot be run (RERR_CMD_RUN = 126).
    CommandRun = 126,

    /// Command not found (RERR_CMD_NOTFOUND = 127).
    ///
    /// Returned when the remote shell or rsync binary is not found.
    CommandNotFound = 127,
}

impl ExitCode {
    /// Returns the numeric exit code value.
    ///
    /// # Examples
    ///
    /// ```
    /// use core::exit_code::ExitCode;
    ///
    /// assert_eq!(ExitCode::Ok.as_i32(), 0);
    /// assert_eq!(ExitCode::PartialTransfer.as_i32(), 23);
    /// ```
    #[must_use]
    pub const fn as_i32(self) -> i32 {
        self as i32
    }

    /// Returns a human-readable description of this exit code.
    ///
    /// These descriptions match upstream rsync's `log.c` error strings.
    ///
    /// # Examples
    ///
    /// ```
    /// use core::exit_code::ExitCode;
    ///
    /// assert_eq!(ExitCode::PartialTransfer.description(), "partial transfer");
    /// ```
    #[must_use]
    pub const fn description(self) -> &'static str {
        match self {
            Self::Ok => "success",
            Self::Syntax => "syntax or usage error",
            Self::Protocol => "protocol incompatibility",
            Self::FileSelect => "errors selecting input/output files, dirs",
            Self::Unsupported => "requested action not supported",
            Self::StartClient => "error starting client-server protocol",
            Self::LogFileAppend => "daemon unable to append to log-file",
            Self::SocketIo => "error in socket IO",
            Self::FileIo => "error in file IO",
            Self::StreamIo => "error in rsync protocol data stream",
            Self::MessageIo => "errors with program diagnostics",
            Self::Ipc => "error in IPC code",
            Self::Crashed => "received SIGSEGV or SIGBUS or SIGABRT",
            Self::Terminated => "received SIGINT, SIGTERM, or SIGHUP",
            Self::Signal1 => "received SIGUSR1",
            Self::Signal => "received SIGINT, SIGTERM, or SIGHUP",
            Self::WaitChild => "waitpid() failed",
            Self::Malloc => "error allocating core memory buffers",
            Self::PartialTransfer => "partial transfer",
            Self::Vanished => "some files vanished before they could be transferred",
            Self::DeleteLimit => "max delete limit stopped deletions",
            Self::Timeout => "timeout in data send/receive",
            Self::ConnectionTimeout => "timeout waiting for daemon connection",
            Self::CommandFailed => "remote command failed",
            Self::CommandKilled => "remote command killed",
            Self::CommandRun => "remote command could not be run",
            Self::CommandNotFound => "remote command not found",
        }
    }

    /// Returns `true` if this represents a successful exit.
    #[must_use]
    pub const fn is_success(self) -> bool {
        matches!(self, Self::Ok)
    }

    /// Returns `true` if this is a fatal error that should stop the transfer.
    #[must_use]
    pub const fn is_fatal(self) -> bool {
        matches!(
            self,
            Self::Protocol
                | Self::StartClient
                | Self::LogFileAppend
                | Self::SocketIo
                | Self::StreamIo
                | Self::Ipc
                | Self::Crashed
                | Self::Terminated
                | Self::Malloc
                | Self::Timeout
                | Self::ConnectionTimeout
        )
    }

    /// Returns `true` if this error indicates a partial transfer occurred.
    #[must_use]
    pub const fn is_partial(self) -> bool {
        matches!(
            self,
            Self::PartialTransfer | Self::Vanished | Self::DeleteLimit
        )
    }

    /// Creates an exit code from an i32 value.
    ///
    /// Returns `None` if the value doesn't correspond to a known exit code.
    ///
    /// # Examples
    ///
    /// ```
    /// use core::exit_code::ExitCode;
    ///
    /// assert_eq!(ExitCode::from_i32(23), Some(ExitCode::PartialTransfer));
    /// assert_eq!(ExitCode::from_i32(999), None);
    /// ```
    #[must_use]
    pub const fn from_i32(value: i32) -> Option<Self> {
        match value {
            0 => Some(Self::Ok),
            1 => Some(Self::Syntax),
            2 => Some(Self::Protocol),
            3 => Some(Self::FileSelect),
            4 => Some(Self::Unsupported),
            5 => Some(Self::StartClient),
            6 => Some(Self::LogFileAppend),
            10 => Some(Self::SocketIo),
            11 => Some(Self::FileIo),
            12 => Some(Self::StreamIo),
            13 => Some(Self::MessageIo),
            14 => Some(Self::Ipc),
            15 => Some(Self::Crashed),
            16 => Some(Self::Terminated),
            19 => Some(Self::Signal1),
            20 => Some(Self::Signal),
            21 => Some(Self::WaitChild),
            22 => Some(Self::Malloc),
            23 => Some(Self::PartialTransfer),
            24 => Some(Self::Vanished),
            25 => Some(Self::DeleteLimit),
            30 => Some(Self::Timeout),
            35 => Some(Self::ConnectionTimeout),
            124 => Some(Self::CommandFailed),
            125 => Some(Self::CommandKilled),
            126 => Some(Self::CommandRun),
            127 => Some(Self::CommandNotFound),
            _ => None,
        }
    }

    /// Maps a `std::io::Error` to an appropriate exit code.
    ///
    /// This helper provides consistent exit code mapping for I/O errors
    /// across the codebase, matching upstream rsync's error handling.
    ///
    /// # Mapping Rules
    ///
    /// - `NotFound`, `PermissionDenied`, `AlreadyExists` → `FileSelect`
    /// - `ConnectionRefused`, `ConnectionReset`, `ConnectionAborted`,
    ///   `BrokenPipe`, `AddrInUse`, `AddrNotAvailable`, `NotConnected` → `SocketIo`
    /// - `TimedOut` → `Timeout`
    /// - `UnexpectedEof`, `InvalidData` → `StreamIo`
    /// - `Interrupted` by signal → `Signal`
    /// - All other I/O errors → `FileIo`
    ///
    /// # Examples
    ///
    /// ```
    /// use core::exit_code::ExitCode;
    /// use std::io::{Error, ErrorKind};
    ///
    /// let err = Error::from(ErrorKind::NotFound);
    /// assert_eq!(ExitCode::from_io_error(&err), ExitCode::FileSelect);
    ///
    /// let err = Error::from(ErrorKind::ConnectionRefused);
    /// assert_eq!(ExitCode::from_io_error(&err), ExitCode::SocketIo);
    ///
    /// let err = Error::from(ErrorKind::TimedOut);
    /// assert_eq!(ExitCode::from_io_error(&err), ExitCode::Timeout);
    /// ```
    #[must_use]
    pub fn from_io_error(error: &std::io::Error) -> Self {
        use std::io::ErrorKind;

        match error.kind() {
            // File selection errors
            ErrorKind::NotFound | ErrorKind::PermissionDenied | ErrorKind::AlreadyExists => {
                Self::FileSelect
            }

            // Network/socket errors
            ErrorKind::ConnectionRefused
            | ErrorKind::ConnectionReset
            | ErrorKind::ConnectionAborted
            | ErrorKind::BrokenPipe
            | ErrorKind::AddrInUse
            | ErrorKind::AddrNotAvailable
            | ErrorKind::NotConnected => Self::SocketIo,

            // Timeout errors
            ErrorKind::TimedOut => Self::Timeout,

            // Protocol/stream errors
            ErrorKind::UnexpectedEof | ErrorKind::InvalidData => Self::StreamIo,

            // Signal interruption
            ErrorKind::Interrupted => Self::Signal,

            // Default to file I/O error for everything else
            _ => Self::FileIo,
        }
    }
}

impl fmt::Display for ExitCode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.description())
    }
}

impl From<ExitCode> for i32 {
    fn from(code: ExitCode) -> Self {
        code.as_i32()
    }
}

impl From<ExitCode> for std::process::ExitCode {
    fn from(code: ExitCode) -> Self {
        // Clamp to u8 range for std::process::ExitCode
        let value = code.as_i32().clamp(0, 255) as u8;
        Self::from(value)
    }
}

/// Returns a human-readable description for a given exit code value.
///
/// This function provides a convenient way to get error descriptions
/// without needing to convert to the `ExitCode` enum first. It returns
/// the description if the code is valid, or a generic "unknown error"
/// message otherwise.
///
/// # Examples
///
/// ```
/// use core::exit_code::exit_code_description;
///
/// assert_eq!(exit_code_description(0), "success");
/// assert_eq!(exit_code_description(23), "partial transfer");
/// assert_eq!(exit_code_description(999), "unknown error code: 999");
/// ```
#[must_use]
pub fn exit_code_description(code: i32) -> String {
    ExitCode::from_i32(code)
        .map(|c| c.description().to_string())
        .unwrap_or_else(|| format!("unknown error code: {code}"))
}

/// Trait for types that have an associated exit code.
///
/// Implement this trait for error types to provide consistent exit
/// code reporting across the workspace.
///
/// # Examples
///
/// ```ignore
/// use core::exit_code::{ExitCode, HasExitCode};
///
/// struct MyError {
///     code: ExitCode,
/// }
///
/// impl HasExitCode for MyError {
///     fn exit_code(&self) -> ExitCode {
///         self.code
///     }
/// }
/// ```
pub trait HasExitCode {
    /// Returns the exit code associated with this value.
    fn exit_code(&self) -> ExitCode;
}

/// Trait for standardized error handling across the workspace.
///
/// This trait provides a unified interface for error types to expose:
/// - Unique error codes for programmatic error identification
/// - Exit codes suitable for process termination (via [`HasExitCode`])
/// - User-friendly error messages for display
///
/// # Design Philosophy
///
/// This trait complements the existing [`HasExitCode`] trait by adding
/// structured error identification and user messaging. It follows these principles:
///
/// - **Error codes** are unique identifiers within an error type, useful for
///   metrics, logging, and programmatic error handling
/// - **Exit codes** (from [`HasExitCode`]) map to upstream rsync's exit codes
///   for process termination
/// - **User messages** provide human-readable descriptions, typically delegating
///   to the `Display` implementation
///
/// # Implementation Guidelines
///
/// When implementing this trait:
///
/// 1. **Error codes** should be unique within your error type. Consider using
///    the discriminant for enums, or a constant for simple error types.
/// 2. **Exit codes** should match upstream rsync's behavior. See [`ExitCode`]
///    for the canonical mapping.
/// 3. **User messages** should be clear and actionable. Include context like
///    file paths, operation names, and underlying error details.
///
/// # Examples
///
/// ```ignore
/// use std::fmt;
/// use core::exit_code::{ExitCode, ErrorCodification, HasExitCode};
/// use thiserror::Error;
///
/// #[derive(Debug, Error)]
/// pub enum MyError {
///     #[error("file not found: {path}")]
///     NotFound { path: String },
///     #[error("permission denied: {path}")]
///     PermissionDenied { path: String },
/// }
///
/// impl HasExitCode for MyError {
///     fn exit_code(&self) -> ExitCode {
///         match self {
///             Self::NotFound { .. } => ExitCode::FileSelect,
///             Self::PermissionDenied { .. } => ExitCode::FileIo,
///         }
///     }
/// }
///
/// impl ErrorCodification for MyError {
///     fn error_code(&self) -> u32 {
///         // Use discriminant or define unique codes
///         match self {
///             Self::NotFound { .. } => 1001,
///             Self::PermissionDenied { .. } => 1002,
///         }
///     }
///
///     fn user_message(&self) -> String {
///         // Delegate to Display implementation
///         self.to_string()
///     }
/// }
/// ```
///
/// # Relationship with `HasExitCode`
///
/// Types implementing `ErrorCodification` should also implement [`HasExitCode`]
/// to provide the exit code. The default implementation of
/// [`ErrorCodification::exit_code_i32`] delegates to `HasExitCode::exit_code().as_i32()`,
/// making it seamless to use both traits together.
pub trait ErrorCodification: HasExitCode + fmt::Display {
    /// Returns a unique error code for this error variant.
    ///
    /// Error codes are used for programmatic error identification, metrics,
    /// and logging. They should be unique within the error type.
    ///
    /// # Guidelines
    ///
    /// - Use the discriminant for enum variants (e.g., `1001`, `1002`, etc.)
    /// - For simple error types, use a constant (e.g., `1000`)
    /// - Document the error code mapping in your error type's documentation
    /// - Ensure codes don't conflict within the same error type
    ///
    /// # Examples
    ///
    /// ```ignore
    /// fn error_code(&self) -> u32 {
    ///     match self {
    ///         Self::NotFound { .. } => 1001,
    ///         Self::PermissionDenied { .. } => 1002,
    ///         Self::IoError { .. } => 1003,
    ///     }
    /// }
    /// ```
    fn error_code(&self) -> u32;

    /// Returns an exit code suitable for process termination.
    ///
    /// This method provides a convenient i32 interface to the exit code,
    /// delegating to the [`HasExitCode`] trait implementation.
    ///
    /// The default implementation calls `self.exit_code().as_i32()`,
    /// ensuring consistency with the typed exit code.
    ///
    /// # Examples
    ///
    /// ```ignore
    /// let err = MyError::NotFound { path: "/tmp/file".into() };
    /// assert_eq!(err.exit_code_i32(), 3); // FileSelect
    /// ```
    fn exit_code_i32(&self) -> i32 {
        self.exit_code().as_i32()
    }

    /// Returns a user-friendly error message.
    ///
    /// This message should be suitable for display to end users. It should:
    /// - Be clear and concise
    /// - Include relevant context (file paths, operation names, etc.)
    /// - Suggest corrective actions when possible
    ///
    /// The default implementation delegates to the `Display` trait, which
    /// is appropriate for most error types using `thiserror::Error`.
    ///
    /// # Examples
    ///
    /// ```ignore
    /// fn user_message(&self) -> String {
    ///     // Default: delegate to Display
    ///     self.to_string()
    /// }
    /// ```
    fn user_message(&self) -> String {
        self.to_string()
    }

    /// Returns the upstream rsync error code name for debugging.
    ///
    /// This method provides the symbolic name of the error code as defined
    /// in upstream rsync's `errcode.h` (e.g., "RERR_SYNTAX", "RERR_PARTIAL").
    ///
    /// The default implementation maps the exit code to its description.
    /// Override this to provide more specific error code names.
    ///
    /// # Examples
    ///
    /// ```ignore
    /// fn error_code_name(&self) -> &'static str {
    ///     match self {
    ///         Self::NotFound { .. } => "RERR_FILESELECT",
    ///         Self::PermissionDenied { .. } => "RERR_FILEIO",
    ///         _ => self.exit_code().description(),
    ///     }
    /// }
    /// ```
    fn error_code_name(&self) -> &'static str {
        self.exit_code().description()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exit_codes_match_upstream() {
        // Verify all codes match upstream errcode.h
        assert_eq!(ExitCode::Ok.as_i32(), 0);
        assert_eq!(ExitCode::Syntax.as_i32(), 1);
        assert_eq!(ExitCode::Protocol.as_i32(), 2);
        assert_eq!(ExitCode::FileSelect.as_i32(), 3);
        assert_eq!(ExitCode::Unsupported.as_i32(), 4);
        assert_eq!(ExitCode::StartClient.as_i32(), 5);
        assert_eq!(ExitCode::LogFileAppend.as_i32(), 6);
        assert_eq!(ExitCode::SocketIo.as_i32(), 10);
        assert_eq!(ExitCode::FileIo.as_i32(), 11);
        assert_eq!(ExitCode::StreamIo.as_i32(), 12);
        assert_eq!(ExitCode::MessageIo.as_i32(), 13);
        assert_eq!(ExitCode::Ipc.as_i32(), 14);
        assert_eq!(ExitCode::Crashed.as_i32(), 15);
        assert_eq!(ExitCode::Terminated.as_i32(), 16);
        assert_eq!(ExitCode::Signal1.as_i32(), 19);
        assert_eq!(ExitCode::Signal.as_i32(), 20);
        assert_eq!(ExitCode::WaitChild.as_i32(), 21);
        assert_eq!(ExitCode::Malloc.as_i32(), 22);
        assert_eq!(ExitCode::PartialTransfer.as_i32(), 23);
        assert_eq!(ExitCode::Vanished.as_i32(), 24);
        assert_eq!(ExitCode::DeleteLimit.as_i32(), 25);
        assert_eq!(ExitCode::Timeout.as_i32(), 30);
        assert_eq!(ExitCode::ConnectionTimeout.as_i32(), 35);
        assert_eq!(ExitCode::CommandFailed.as_i32(), 124);
        assert_eq!(ExitCode::CommandKilled.as_i32(), 125);
        assert_eq!(ExitCode::CommandRun.as_i32(), 126);
        assert_eq!(ExitCode::CommandNotFound.as_i32(), 127);
    }

    #[test]
    fn from_i32_roundtrips() {
        for code in [
            ExitCode::Ok,
            ExitCode::Syntax,
            ExitCode::Protocol,
            ExitCode::FileSelect,
            ExitCode::PartialTransfer,
            ExitCode::Timeout,
            ExitCode::CommandNotFound,
        ] {
            let value = code.as_i32();
            assert_eq!(ExitCode::from_i32(value), Some(code));
        }
    }

    #[test]
    fn from_i32_returns_none_for_unknown() {
        assert_eq!(ExitCode::from_i32(-1), None);
        assert_eq!(ExitCode::from_i32(7), None); // 7-9 are not used
        assert_eq!(ExitCode::from_i32(100), None);
        assert_eq!(ExitCode::from_i32(999), None);
    }

    #[test]
    fn is_success_only_for_ok() {
        assert!(ExitCode::Ok.is_success());
        assert!(!ExitCode::Syntax.is_success());
        assert!(!ExitCode::PartialTransfer.is_success());
    }

    #[test]
    fn is_fatal_for_critical_errors() {
        assert!(ExitCode::Protocol.is_fatal());
        assert!(ExitCode::SocketIo.is_fatal());
        assert!(ExitCode::Timeout.is_fatal());
        assert!(ExitCode::Crashed.is_fatal());

        assert!(!ExitCode::Ok.is_fatal());
        assert!(!ExitCode::PartialTransfer.is_fatal());
        assert!(!ExitCode::Syntax.is_fatal());
    }

    #[test]
    fn is_partial_for_partial_errors() {
        assert!(ExitCode::PartialTransfer.is_partial());
        assert!(ExitCode::Vanished.is_partial());
        assert!(ExitCode::DeleteLimit.is_partial());

        assert!(!ExitCode::Ok.is_partial());
        assert!(!ExitCode::Protocol.is_partial());
    }

    #[test]
    fn display_shows_description() {
        assert_eq!(format!("{}", ExitCode::Ok), "success");
        assert_eq!(format!("{}", ExitCode::PartialTransfer), "partial transfer");
    }

    #[test]
    fn into_i32_conversion() {
        let code: i32 = ExitCode::PartialTransfer.into();
        assert_eq!(code, 23);
    }

    #[test]
    fn into_process_exit_code() {
        let code: std::process::ExitCode = ExitCode::PartialTransfer.into();
        // We can't directly compare ExitCode values, but we can verify it compiles
        let _ = code;
    }

    #[test]
    fn exit_code_is_copy() {
        let code = ExitCode::Ok;
        let copy = code;
        assert_eq!(code, copy);
    }

    #[test]
    fn exit_code_is_debug() {
        let debug = format!("{:?}", ExitCode::PartialTransfer);
        assert!(debug.contains("PartialTransfer"));
    }

    #[test]
    fn exit_code_is_hashable() {
        use std::collections::HashSet;
        let mut set = HashSet::new();
        set.insert(ExitCode::Ok);
        set.insert(ExitCode::PartialTransfer);
        assert_eq!(set.len(), 2);
    }

    #[test]
    fn descriptions_are_not_empty() {
        for code in [
            ExitCode::Ok,
            ExitCode::Syntax,
            ExitCode::Protocol,
            ExitCode::FileSelect,
            ExitCode::Unsupported,
            ExitCode::StartClient,
            ExitCode::LogFileAppend,
            ExitCode::SocketIo,
            ExitCode::FileIo,
            ExitCode::StreamIo,
            ExitCode::MessageIo,
            ExitCode::Ipc,
            ExitCode::Crashed,
            ExitCode::Terminated,
            ExitCode::Signal1,
            ExitCode::Signal,
            ExitCode::WaitChild,
            ExitCode::Malloc,
            ExitCode::PartialTransfer,
            ExitCode::Vanished,
            ExitCode::DeleteLimit,
            ExitCode::Timeout,
            ExitCode::ConnectionTimeout,
            ExitCode::CommandFailed,
            ExitCode::CommandKilled,
            ExitCode::CommandRun,
            ExitCode::CommandNotFound,
        ] {
            assert!(
                !code.description().is_empty(),
                "Empty description for {code:?}"
            );
        }
    }

    #[test]
    fn from_io_error_maps_file_errors() {
        use std::io::{Error, ErrorKind};

        let err = Error::from(ErrorKind::NotFound);
        assert_eq!(ExitCode::from_io_error(&err), ExitCode::FileSelect);

        let err = Error::from(ErrorKind::PermissionDenied);
        assert_eq!(ExitCode::from_io_error(&err), ExitCode::FileSelect);

        let err = Error::from(ErrorKind::AlreadyExists);
        assert_eq!(ExitCode::from_io_error(&err), ExitCode::FileSelect);
    }

    #[test]
    fn from_io_error_maps_network_errors() {
        use std::io::{Error, ErrorKind};

        let err = Error::from(ErrorKind::ConnectionRefused);
        assert_eq!(ExitCode::from_io_error(&err), ExitCode::SocketIo);

        let err = Error::from(ErrorKind::ConnectionReset);
        assert_eq!(ExitCode::from_io_error(&err), ExitCode::SocketIo);

        let err = Error::from(ErrorKind::ConnectionAborted);
        assert_eq!(ExitCode::from_io_error(&err), ExitCode::SocketIo);

        let err = Error::from(ErrorKind::BrokenPipe);
        assert_eq!(ExitCode::from_io_error(&err), ExitCode::SocketIo);

        let err = Error::from(ErrorKind::AddrInUse);
        assert_eq!(ExitCode::from_io_error(&err), ExitCode::SocketIo);

        let err = Error::from(ErrorKind::AddrNotAvailable);
        assert_eq!(ExitCode::from_io_error(&err), ExitCode::SocketIo);

        let err = Error::from(ErrorKind::NotConnected);
        assert_eq!(ExitCode::from_io_error(&err), ExitCode::SocketIo);
    }

    #[test]
    fn from_io_error_maps_timeout_errors() {
        use std::io::{Error, ErrorKind};

        let err = Error::from(ErrorKind::TimedOut);
        assert_eq!(ExitCode::from_io_error(&err), ExitCode::Timeout);
    }

    #[test]
    fn from_io_error_maps_stream_errors() {
        use std::io::{Error, ErrorKind};

        let err = Error::from(ErrorKind::UnexpectedEof);
        assert_eq!(ExitCode::from_io_error(&err), ExitCode::StreamIo);

        let err = Error::from(ErrorKind::InvalidData);
        assert_eq!(ExitCode::from_io_error(&err), ExitCode::StreamIo);
    }

    #[test]
    fn from_io_error_maps_signal_interruption() {
        use std::io::{Error, ErrorKind};

        let err = Error::from(ErrorKind::Interrupted);
        assert_eq!(ExitCode::from_io_error(&err), ExitCode::Signal);
    }

    #[test]
    fn from_io_error_defaults_to_file_io() {
        use std::io::{Error, ErrorKind};

        // Test various error kinds that should default to FileIo
        for kind in [
            ErrorKind::WriteZero,
            ErrorKind::InvalidInput,
            ErrorKind::Other,
            ErrorKind::OutOfMemory,
        ] {
            let err = Error::from(kind);
            assert_eq!(
                ExitCode::from_io_error(&err),
                ExitCode::FileIo,
                "ErrorKind::{kind:?} should map to FileIo"
            );
        }
    }

    #[test]
    fn exit_code_description_function_works() {
        assert_eq!(exit_code_description(0), "success");
        assert_eq!(exit_code_description(1), "syntax or usage error");
        assert_eq!(exit_code_description(23), "partial transfer");
        assert_eq!(
            exit_code_description(6),
            "daemon unable to append to log-file"
        );
    }

    #[test]
    fn exit_code_description_handles_unknown() {
        assert_eq!(exit_code_description(999), "unknown error code: 999");
        assert_eq!(exit_code_description(-1), "unknown error code: -1");
        assert_eq!(exit_code_description(7), "unknown error code: 7");
    }

    #[test]
    fn all_upstream_exit_codes_present() {
        // Verify all exit codes from the upstream rsync man page are present
        // This ensures we maintain compatibility with rsync 3.4.1

        // From the man page and errcode.h:
        assert_eq!(ExitCode::Ok.as_i32(), 0, "Success");
        assert_eq!(ExitCode::Syntax.as_i32(), 1, "Syntax or usage error");
        assert_eq!(ExitCode::Protocol.as_i32(), 2, "Protocol incompatibility");
        assert_eq!(
            ExitCode::FileSelect.as_i32(),
            3,
            "Errors selecting input/output files, dirs"
        );
        assert_eq!(
            ExitCode::Unsupported.as_i32(),
            4,
            "Requested action not supported"
        );
        assert_eq!(
            ExitCode::StartClient.as_i32(),
            5,
            "Error starting client-server protocol"
        );
        assert_eq!(
            ExitCode::LogFileAppend.as_i32(),
            6,
            "Daemon unable to append to log-file"
        );
        assert_eq!(ExitCode::SocketIo.as_i32(), 10, "Error in socket I/O");
        assert_eq!(ExitCode::FileIo.as_i32(), 11, "Error in file I/O");
        assert_eq!(
            ExitCode::StreamIo.as_i32(),
            12,
            "Error in rsync protocol data stream"
        );
        assert_eq!(
            ExitCode::MessageIo.as_i32(),
            13,
            "Errors with program diagnostics"
        );
        assert_eq!(ExitCode::Ipc.as_i32(), 14, "Error in IPC code");
        assert_eq!(
            ExitCode::Signal.as_i32(),
            20,
            "Received SIGUSR1 or SIGINT"
        );
        assert_eq!(
            ExitCode::WaitChild.as_i32(),
            21,
            "Some error returned by waitpid()"
        );
        assert_eq!(
            ExitCode::Malloc.as_i32(),
            22,
            "Error allocating core memory buffers"
        );
        assert_eq!(
            ExitCode::PartialTransfer.as_i32(),
            23,
            "Partial transfer due to error"
        );
        assert_eq!(
            ExitCode::Vanished.as_i32(),
            24,
            "Partial transfer due to vanished source files"
        );
        assert_eq!(
            ExitCode::DeleteLimit.as_i32(),
            25,
            "The --max-delete limit stopped deletions"
        );
        assert_eq!(
            ExitCode::Timeout.as_i32(),
            30,
            "Timeout in data send/receive"
        );
        assert_eq!(
            ExitCode::ConnectionTimeout.as_i32(),
            35,
            "Timeout waiting for daemon connection"
        );
    }

    #[test]
    fn from_i32_covers_all_exit_codes() {
        // Ensure every ExitCode variant can be round-tripped through from_i32
        let all_codes = [
            ExitCode::Ok,
            ExitCode::Syntax,
            ExitCode::Protocol,
            ExitCode::FileSelect,
            ExitCode::Unsupported,
            ExitCode::StartClient,
            ExitCode::LogFileAppend,
            ExitCode::SocketIo,
            ExitCode::FileIo,
            ExitCode::StreamIo,
            ExitCode::MessageIo,
            ExitCode::Ipc,
            ExitCode::Crashed,
            ExitCode::Terminated,
            ExitCode::Signal1,
            ExitCode::Signal,
            ExitCode::WaitChild,
            ExitCode::Malloc,
            ExitCode::PartialTransfer,
            ExitCode::Vanished,
            ExitCode::DeleteLimit,
            ExitCode::Timeout,
            ExitCode::ConnectionTimeout,
            ExitCode::CommandFailed,
            ExitCode::CommandKilled,
            ExitCode::CommandRun,
            ExitCode::CommandNotFound,
        ];

        for code in all_codes {
            let value = code.as_i32();
            assert_eq!(
                ExitCode::from_i32(value),
                Some(code),
                "Failed to round-trip {code:?} (value: {value})"
            );
        }
    }
}
