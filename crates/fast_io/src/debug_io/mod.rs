//! DEBUG_IO tracing support for I/O operations.
//!
//! This module provides debug tracing at four levels, mirroring upstream rsync's DEBUG_IO:
//!
//! - **Level 1**: Basic I/O operations (open, close)
//! - **Level 2**: Read/write operations with sizes
//! - **Level 3**: Buffer management (pool acquire/release, buffer state)
//! - **Level 4**: Detailed byte-level tracing (hex dumps, byte patterns)
//!
//! # Usage
//!
//! Enable tracing by setting the `tracing` feature and using the appropriate
//! debug level with rsync's `--debug=io` flag (e.g., `--debug=io4` for level 4).
//!
//! ```rust,ignore
//! use fast_io::debug_io;
//!
//! // Level 1: Basic I/O operations
//! debug_io::trace_open("/path/to/file", 1024);
//! debug_io::trace_close("/path/to/file");
//!
//! // Level 2: Read/write with sizes
//! debug_io::trace_read("/path/to/file", 512, 1024);
//! debug_io::trace_write("/path/to/file", 256);
//!
//! // Level 3: Buffer management
//! debug_io::trace_buffer_acquire(4096, 3);
//! debug_io::trace_buffer_release(4096);
//!
//! // Level 4: Byte-level details
//! debug_io::trace_bytes_read(&data[..32], 0);
//! debug_io::trace_bytes_written(&data[..32], 0);
//! ```
//!
//! # Integration with rsync logging
//!
//! When the `tracing` feature is enabled, this module emits tracing events with
//! the target `rsync::io`, which integrates with rsync's debug flag system.
//! The tracing level maps to rsync's `--debug=io` levels 1-4.

pub mod format;
pub mod io_uring_traces;
pub mod operations;
pub mod transfers;

pub use format::{
    trace_buffer_acquire, trace_buffer_pool_create, trace_buffer_release, trace_buffer_state,
    trace_bytes_read, trace_bytes_written, trace_data_pattern,
};
pub use io_uring_traces::{
    trace_io_uring_complete, trace_io_uring_submit, trace_mmap, trace_mmap_advise, trace_munmap,
};
pub use operations::{trace_close, trace_create, trace_open};
pub use transfers::{trace_read, trace_seek, trace_sync, trace_write};

/// Maximum bytes to include in level 4 hex dumps.
pub const MAX_HEX_DUMP_BYTES: usize = 64;

#[cfg(test)]
mod tests;
