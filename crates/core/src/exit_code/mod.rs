//! Centralized exit code definitions matching upstream rsync.
//!
//! This module provides a unified [`ExitCode`] enum that mirrors the exit codes
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

mod codes;
mod convert;
mod traits;

pub use codes::ExitCode;
pub use traits::{ErrorCodification, HasExitCode};

#[cfg(test)]
mod tests;

/// Returns a human-readable description for a given exit code value.
///
/// Provides a convenient way to get error descriptions without converting
/// to the `ExitCode` enum first. Returns the description if the code is
/// valid, or a generic "unknown error" message otherwise.
#[must_use]
pub fn exit_code_description(code: i32) -> String {
    ExitCode::from_i32(code)
        .map(|c| c.description().to_string())
        .unwrap_or_else(|| format!("unknown error code: {code}"))
}
