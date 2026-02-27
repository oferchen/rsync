//! Async file I/O operations for the engine crate.
//!
//! This module provides tokio-based async alternatives to synchronous file
//! operations. It is only available when the `async` feature is enabled.
//!
//! # Features
//!
//! - Async file reading and writing with configurable buffer sizes
//! - Async file copying with progress callbacks
//! - Async checksum computation using spawn_blocking for CPU-intensive work
//! - Async sparse file writing support
//!
//! # Example
//!
//! ```ignore
//! use engine::async_io::{AsyncFileCopier, CopyProgress};
//!
//! let copier = AsyncFileCopier::new()
//!     .with_buffer_size(64 * 1024)
//!     .with_progress(|progress| {
//!         println!("Copied {} bytes", progress.bytes_copied);
//!     });
//!
//! copier.copy_file(source, destination).await?;
//! ```

mod batch;
mod checksum;
mod copier;
mod error;
mod progress;
mod reader;
mod writer;

/// Default buffer size for async file operations (64 KB).
pub const DEFAULT_BUFFER_SIZE: usize = 64 * 1024;

pub use batch::AsyncBatchCopier;
pub use checksum::{ChecksumAlgorithm, compute_file_checksum};
pub use copier::AsyncFileCopier;
pub use error::AsyncIoError;
pub use progress::{CopyProgress, CopyResult};
pub use reader::AsyncFileReader;
pub use writer::AsyncFileWriter;
