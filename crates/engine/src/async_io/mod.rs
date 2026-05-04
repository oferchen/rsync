//! Async file I/O operations for the engine crate.
//!
//! This module provides tokio-based async alternatives to synchronous file
//! operations. It is only available when the `async` feature is enabled.
//!
//! # Features
//!
//! - Async file copying with progress callbacks
//! - Async batch copying with bounded concurrency
//! - Async sparse file writing support
//!
//! # Example
//!
//! ```ignore
//! use engine::async_io::{AsyncFileCopier, CopyProgress};
//!
//! let copier = AsyncFileCopier::new()
//!     .with_buffer_size(64 * 1024);
//!
//! copier.copy_file(source, destination).await?;
//! ```

mod batch;
mod copier;
mod error;
mod progress;

/// Default buffer size for async file operations (64 KB).
pub const DEFAULT_BUFFER_SIZE: usize = 64 * 1024;

pub use batch::AsyncBatchCopier;
pub use copier::AsyncFileCopier;
pub use error::AsyncIoError;
pub use progress::{CopyProgress, CopyResult};
