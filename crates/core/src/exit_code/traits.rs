use std::fmt;

use super::ExitCode;

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
/// Provides a unified interface for error types to expose:
/// - Unique error codes for programmatic error identification
/// - Exit codes suitable for process termination (via [`HasExitCode`])
/// - User-friendly error messages for display
///
/// # Relationship with `HasExitCode`
///
/// Types implementing `ErrorCodification` must also implement [`HasExitCode`]
/// to provide the exit code. The default implementation of
/// [`ErrorCodification::exit_code_i32`] delegates to `HasExitCode::exit_code().as_i32()`.
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
///         match self {
///             Self::NotFound { .. } => 1001,
///             Self::PermissionDenied { .. } => 1002,
///         }
///     }
///
///     fn user_message(&self) -> String {
///         self.to_string()
///     }
/// }
/// ```
pub trait ErrorCodification: HasExitCode + fmt::Display {
    /// Returns a unique error code for this error variant.
    ///
    /// Error codes are used for programmatic error identification, metrics,
    /// and logging. They should be unique within the error type.
    fn error_code(&self) -> u32;

    /// Returns an exit code suitable for process termination as i32.
    ///
    /// The default implementation delegates to `self.exit_code().as_i32()`.
    fn exit_code_i32(&self) -> i32 {
        self.exit_code().as_i32()
    }

    /// Returns a user-friendly error message.
    ///
    /// Should be suitable for display to end users. The default
    /// implementation delegates to the `Display` trait.
    fn user_message(&self) -> String {
        self.to_string()
    }

    /// Returns the upstream rsync error code name for debugging.
    ///
    /// Provides the symbolic name as defined in upstream rsync's `errcode.h`
    /// (e.g., "RERR_SYNTAX", "RERR_PARTIAL"). The default implementation
    /// maps the exit code to its description.
    fn error_code_name(&self) -> &'static str {
        self.exit_code().description()
    }
}
