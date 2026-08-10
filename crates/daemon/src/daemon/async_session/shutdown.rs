//! Error and shutdown types for async daemon operations.
//!
//! [`AsyncDaemonError`] covers I/O failures, connection limits, timeouts,
//! shutdown signals, and protocol errors encountered during async session
//! handling.

use std::io;
use std::time::Duration;

/// Error type for async daemon operations.
#[derive(Debug)]
pub enum AsyncDaemonError {
    /// I/O error during daemon operation.
    Io(io::Error),

    /// Connection limit reached.
    ConnectionLimitReached(usize),

    /// Session timeout.
    Timeout(Duration),

    /// Shutdown signal received.
    Shutdown,

    /// Protocol error.
    Protocol(String),
}

impl std::fmt::Display for AsyncDaemonError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(e) => write!(f, "I/O error: {e}"),
            Self::ConnectionLimitReached(max) => {
                write!(f, "Maximum connections ({max}) reached")
            }
            Self::Timeout(d) => write!(f, "Session timed out after {d:?}"),
            Self::Shutdown => write!(f, "Daemon shutdown requested"),
            Self::Protocol(msg) => write!(f, "Protocol error: {msg}"),
        }
    }
}

impl std::error::Error for AsyncDaemonError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io(e) => Some(e),
            _ => None,
        }
    }
}

impl From<io::Error> for AsyncDaemonError {
    fn from(e: io::Error) -> Self {
        Self::Io(e)
    }
}
