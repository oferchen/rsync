//! High-performance I/O abstractions for rsync.
//!
//! This crate provides optimized I/O primitives that leverage modern OS features
//! and parallelism to maximize throughput.
//!
//! # Features
//!
//! - **Parallel file operations** using rayon for multi-core utilization
//! - **Memory-mapped I/O** for large files with runtime fallback to buffered I/O
//! - **Zero-copy file transfer** using `copy_file_range` for file-to-file copies
//! - **Zero-copy socket send** using `sendfile` for file-to-socket transfers
//! - **io_uring** for batched syscalls on Linux (optional, `io_uring` feature)
//! - **Buffer pools** for reduced allocation overhead
//! - **Cached sorting** with Schwartzian transform
//!
//! # Design Principles
//!
//! 1. **Zero-copy where possible** - Use mmap, sendfile, and buffer reuse
//! 2. **Batch operations** - Reduce syscall overhead
//! 3. **Parallel by default** - Utilize all CPU cores
//! 4. **Graceful fallback** - Work on all platforms, fall back to buffered I/O
//!    when specialized syscalls are unavailable (NFS, FUSE, old kernels, etc.)

#![deny(rustdoc::broken_intra_doc_links)]
#![deny(missing_docs)]

pub mod buffer_pool;
pub mod cached_sort;
pub mod parallel;
pub mod traits;

// Unix-only modules that depend on unix-specific APIs
#[cfg(unix)]
pub mod copy_file_range;
#[cfg(unix)]
pub mod mmap_reader;
#[cfg(unix)]
pub mod sendfile;
#[cfg(unix)]
pub mod syscall_batch;

/// io_uring-based async file I/O for Linux 5.6+.
///
/// This module is only available on Linux with the `io_uring` feature enabled.
/// It provides high-performance file I/O with automatic fallback to standard I/O
/// on unsupported systems.
#[cfg(all(target_os = "linux", feature = "io_uring"))]
pub mod io_uring;

pub use buffer_pool::{BufferGuard, BufferPool};
pub use cached_sort::{CachedSortKey, cached_sort_by};
pub use parallel::{ParallelExecutor, ParallelResult};
pub use traits::{FileReader, FileWriter};

#[cfg(unix)]
pub use mmap_reader::MmapReader;

#[cfg(all(target_os = "linux", feature = "io_uring"))]
pub use io_uring::{
    IoUringConfig, IoUringOrStdReader, IoUringOrStdWriter, IoUringReader, IoUringReaderFactory,
    IoUringWriter, IoUringWriterFactory, is_io_uring_available,
};
