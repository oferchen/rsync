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
    Io(
        #[from]
        #[source]
        io::Error,
    ),
    /// Invalid configuration or parameters.
    #[error("Invalid configuration: {0}")]
    InvalidConfig(String),
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
        let engine_err: EngineError = io_err.into();

        assert!(matches!(engine_err, EngineError::Io(_)));
        assert!(engine_err.to_string().contains("I/O error"));
    }

    #[test]
    fn invalid_config_error() {
        let err = EngineError::InvalidConfig("missing required field".to_string());

        assert!(matches!(err, EngineError::InvalidConfig(_)));
        assert!(err.to_string().contains("Invalid configuration"));
        assert!(err.to_string().contains("missing required field"));
    }

    #[test]
    fn unsupported_error() {
        let err = EngineError::Unsupported("feature X".to_string());

        assert!(matches!(err, EngineError::Unsupported(_)));
        assert!(err.to_string().contains("Unsupported operation"));
        assert!(err.to_string().contains("feature X"));
    }

    #[test]
    fn debug_format() {
        let err = EngineError::InvalidConfig("test".to_string());
        let debug = format!("{:?}", err);

        assert!(debug.contains("InvalidConfig"));
    }

    #[test]
    fn error_source_for_io() {
        use std::error::Error;

        let io_err = io::Error::new(ErrorKind::PermissionDenied, "access denied");
        let engine_err: EngineError = io_err.into();

        assert!(engine_err.source().is_some());
    }

    #[test]
    fn engine_result_ok() {
        let result: EngineResult<i32> = Ok(42);
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), 42);
    }

    #[test]
    fn engine_result_err() {
        let result: EngineResult<i32> = Err(EngineError::Unsupported("test".into()));
        assert!(result.is_err());
    }
}
