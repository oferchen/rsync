//! Builder pattern for constructing [`LocalCopyOptions`](crate::local_copy::LocalCopyOptions).
//!
//! This module provides [`LocalCopyOptionsBuilder`], a fluent API for constructing
//! [`LocalCopyOptions`](crate::local_copy::LocalCopyOptions) with validation at build time.
//!
//! # Example
//!
//! ```rust
//! use engine::local_copy::LocalCopyOptions;
//!
//! let options = LocalCopyOptions::builder()
//!     .recursive(true)
//!     .preserve_times(true)
//!     .preserve_permissions(true)
//!     .delete(true)
//!     .build()
//!     .expect("valid options");
//! ```

mod definition;
mod error;
mod setters_deletion;
mod setters_metadata;
mod setters_misc;
mod setters_path;
mod setters_staging;
mod setters_transfer;
mod validation;

#[cfg(test)]
mod tests;

pub use definition::LocalCopyOptionsBuilder;
pub use error::BuilderError;
