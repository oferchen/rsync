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
        assert_eq!(ExitCode::from_i32(6), None);
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
}
