//! Common error types for the engine crate.

use std::io;

use thiserror::Error;

/// Result type for engine operations.
pub type EngineResult<T> = Result<T, EngineError>;

/// Errors that can occur during engine operations.
#[derive(Debug, Error)]
pub enum EngineError {
    /// I/O error occurred.
    #[error("I/O error: {0}")]
    Io(#[from] #[source] io::Error),
    /// Invalid configuration or parameters.
    #[error("Invalid configuration: {0}")]
    InvalidConfig(String),
    /// Operation not supported.
    #[error("Unsupported operation: {0}")]
    Unsupported(String),
}
