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
    #[must_use]
    pub const fn as_i32(self) -> i32 {
        self as i32
    }

    /// Returns a human-readable description of this exit code.
    ///
    /// These descriptions match upstream rsync's `log.c` error strings.
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
    /// Provides consistent exit code mapping for I/O errors across the
    /// codebase, matching upstream rsync's error handling.
    ///
    /// # Mapping Rules
    ///
    /// - `NotFound`, `PermissionDenied`, `AlreadyExists` - `FileSelect`
    /// - `ConnectionRefused`, `ConnectionReset`, `ConnectionAborted`,
    ///   `BrokenPipe`, `AddrInUse`, `AddrNotAvailable`, `NotConnected` - `SocketIo`
    /// - `TimedOut` - `Timeout`
    /// - `UnexpectedEof`, `InvalidData` - `StreamIo`
    /// - `Interrupted` by signal - `Signal`
    /// - All other I/O errors - `FileIo`
    #[must_use]
    pub fn from_io_error(error: &std::io::Error) -> Self {
        use std::io::ErrorKind;

        match error.kind() {
            ErrorKind::NotFound | ErrorKind::PermissionDenied | ErrorKind::AlreadyExists => {
                Self::FileSelect
            }

            ErrorKind::ConnectionRefused
            | ErrorKind::ConnectionReset
            | ErrorKind::ConnectionAborted
            | ErrorKind::BrokenPipe
            | ErrorKind::AddrInUse
            | ErrorKind::AddrNotAvailable
            | ErrorKind::NotConnected => Self::SocketIo,

            ErrorKind::TimedOut => Self::Timeout,

            ErrorKind::UnexpectedEof | ErrorKind::InvalidData => Self::StreamIo,

            ErrorKind::Interrupted => Self::Signal,

            _ => Self::FileIo,
        }
    }
}
