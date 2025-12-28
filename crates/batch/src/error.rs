//! crates/batch/src/error.rs
//!
//! Error types for batch operations.

use std::io;

use thiserror::Error;

/// Result type for batch operations.
pub type BatchResult<T> = Result<T, BatchError>;

/// Errors that can occur during batch operations.
#[derive(Debug, Error)]
pub enum BatchError {
    /// I/O error occurred.
    #[error("I/O error: {0}")]
    Io(
        #[from]
        #[source]
        io::Error,
    ),
    /// Invalid batch file format.
    #[error("Invalid batch format: {0}")]
    InvalidFormat(String),
    /// Operation not supported.
    #[error("Unsupported operation: {0}")]
    Unsupported(String),
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::ErrorKind;

    #[test]
    fn io_error_from_std_io_error() {
        let io_err = io::Error::new(ErrorKind::NotFound, "file not found");
        let batch_err: BatchError = io_err.into();

        assert!(matches!(batch_err, BatchError::Io(_)));
        assert!(batch_err.to_string().contains("I/O error"));
    }

    #[test]
    fn invalid_format_error() {
        let err = BatchError::InvalidFormat("corrupted header".to_owned());

        assert!(matches!(err, BatchError::InvalidFormat(_)));
        assert!(err.to_string().contains("Invalid batch format"));
        assert!(err.to_string().contains("corrupted header"));
    }

    #[test]
    fn unsupported_error() {
        let err = BatchError::Unsupported("feature X".to_owned());

        assert!(matches!(err, BatchError::Unsupported(_)));
        assert!(err.to_string().contains("Unsupported operation"));
        assert!(err.to_string().contains("feature X"));
    }

    #[test]
    fn debug_format() {
        let err = BatchError::InvalidFormat("test".to_owned());
        let debug = format!("{err:?}");

        assert!(debug.contains("InvalidFormat"));
    }

    #[test]
    fn error_source_for_io() {
        use std::error::Error;

        let io_err = io::Error::new(ErrorKind::PermissionDenied, "access denied");
        let batch_err: BatchError = io_err.into();

        assert!(batch_err.source().is_some());
    }

    #[test]
    fn batch_result_ok() {
        let result: BatchResult<i32> = Ok(42);
        assert!(result.is_ok());
        let Ok(value) = result else {
            panic!("expected Ok")
        };
        assert_eq!(value, 42);
    }

    #[test]
    fn batch_result_err() {
        let result: BatchResult<i32> = Err(BatchError::Unsupported("test".into()));
        assert!(result.is_err());
    }
}
