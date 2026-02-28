//! Error types for async I/O operations.

use std::io;
use std::path::PathBuf;

use tokio::task;

/// Error type for async file operations.
#[derive(Debug, thiserror::Error)]
pub enum AsyncIoError {
    /// I/O error during file operation.
    #[error("I/O error at {path}: {source}")]
    Io {
        /// The path where the error occurred.
        path: PathBuf,
        /// The underlying I/O error.
        #[source]
        source: io::Error,
    },

    /// Error joining a spawned task.
    #[error("Task join error: {0}")]
    JoinError(#[from] task::JoinError),

    /// Operation was cancelled before completion.
    #[error("Operation cancelled for {0}")]
    Cancelled(PathBuf),
}

impl AsyncIoError {
    /// Creates an I/O error with path context.
    pub fn io(path: impl Into<PathBuf>, source: io::Error) -> Self {
        Self::Io {
            path: path.into(),
            source,
        }
    }
}

/// Extension trait for mapping I/O results to `AsyncIoError` with path context.
///
/// This reduces boilerplate when converting `io::Result<T>` to `Result<T, AsyncIoError>`.
pub(crate) trait IoResultExt<T> {
    /// Maps an I/O error to `AsyncIoError::Io` with the given path.
    fn with_path(self, path: impl Into<PathBuf>) -> Result<T, AsyncIoError>;
}

impl<T> IoResultExt<T> for io::Result<T> {
    fn with_path(self, path: impl Into<PathBuf>) -> Result<T, AsyncIoError> {
        self.map_err(|e| AsyncIoError::io(path, e))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_async_io_error_io() {
        let error = AsyncIoError::io(
            "/path/to/file",
            io::Error::new(io::ErrorKind::NotFound, "not found"),
        );
        let display = format!("{error}");
        assert!(display.contains("/path/to/file"));
    }

    #[test]
    fn test_async_io_error_cancelled() {
        let error = AsyncIoError::Cancelled(PathBuf::from("/cancelled/file"));
        let display = format!("{error}");
        assert!(display.contains("cancelled"));
        assert!(display.contains("/cancelled/file"));
    }
}
