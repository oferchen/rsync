//! Common error types for the engine crate.

use std::fmt;
use std::io;

/// Result type for engine operations.
pub type EngineResult<T> = Result<T, EngineError>;

/// Errors that can occur during engine operations.
#[derive(Debug)]
pub enum EngineError {
    /// I/O error occurred.
    Io(io::Error),
    /// Invalid configuration or parameters.
    InvalidConfig(String),
    /// Operation not supported.
    Unsupported(String),
}

impl fmt::Display for EngineError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(e) => write!(f, "I/O error: {}", e),
            Self::InvalidConfig(msg) => write!(f, "Invalid configuration: {}", msg),
            Self::Unsupported(msg) => write!(f, "Unsupported operation: {}", msg),
        }
    }
}

impl std::error::Error for EngineError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io(e) => Some(e),
            _ => None,
        }
    }
}

impl From<io::Error> for EngineError {
    fn from(err: io::Error) -> Self {
        Self::Io(err)
    }
}
