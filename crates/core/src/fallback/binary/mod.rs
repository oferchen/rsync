//! Binary availability checking for fallback execution.
//!
//! This module provides utilities for locating and verifying executable binaries
//! that can be used as fallback implementations when delegating operations to
//! the system rsync binary. It handles PATH resolution, executable verification,
//! and diagnostic messages for missing binaries.
//!
//! # Core Functionality
//!
//! - [`fallback_binary_path`] - Resolves a binary name to an executable path
//! - [`fallback_binary_available`] - Checks if a binary exists and is executable
//! - [`fallback_binary_is_self`] - Prevents infinite recursion by detecting self-references
//! - [`fallback_binary_candidates`] - Enumerates potential executable locations
//! - [`describe_missing_fallback_binary`] - Generates user-friendly error messages
//!
//! # Platform Support
//!
//! The module adapts to platform-specific executable resolution:
//! - **Unix**: Uses execute bit permissions to verify executability
//! - **Windows**: Honors PATHEXT for extension-based executable lookup
//!
//! # Caching
//!
//! Binary availability results are cached to avoid repeated filesystem operations.
//! The cache is automatically invalidated when environment variables (PATH, PATHEXT)
//! change or when previously-found executables become unavailable.

mod availability;
mod candidates;
mod diagnostics;
#[cfg(unix)]
mod unix;

pub use self::availability::{
    fallback_binary_available, fallback_binary_is_self, fallback_binary_path,
};
pub use self::candidates::fallback_binary_candidates;
pub use self::diagnostics::describe_missing_fallback_binary;

#[cfg(test)]
mod tests;
