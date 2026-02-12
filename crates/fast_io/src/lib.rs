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

// These modules already handle all platforms internally via #[cfg] blocks:
// - copy_file_range: uses Linux copy_file_range syscall, falls back to read/write
// - sendfile: uses Linux sendfile syscall, falls back to read/write
// - syscall_batch: uses Linux statx + libc, falls back to std::fs + filetime
pub mod copy_file_range;
pub mod sendfile;
pub mod syscall_batch;

// mmap_reader depends on memmap2 (Unix-only dependency), so needs a stub
#[cfg(unix)]
pub mod mmap_reader;
#[cfg(not(unix))]
#[path = "mmap_reader_stub.rs"]
pub mod mmap_reader;

/// io_uring-based async file I/O for Linux 5.6+.
///
/// This module provides high-performance file I/O using Linux's io_uring interface
/// with automatic fallback to standard I/O on unsupported systems. On non-Linux
/// platforms or without the `io_uring` feature, a stub is used that always falls
/// back to standard I/O.
#[cfg(all(target_os = "linux", feature = "io_uring"))]
pub mod io_uring;
#[cfg(not(all(target_os = "linux", feature = "io_uring")))]
#[path = "io_uring_stub.rs"]
pub mod io_uring;

pub use buffer_pool::{BufferGuard, BufferPool};
pub use cached_sort::{CachedSortKey, cached_sort_by};
pub use parallel::{ParallelExecutor, ParallelResult};
pub use traits::{FileReader, FileWriter};

pub use mmap_reader::MmapReader;

pub use io_uring::{
    IoUringConfig, IoUringOrStdReader, IoUringOrStdWriter, IoUringReader, IoUringReaderFactory,
    IoUringWriter, IoUringWriterFactory, is_io_uring_available,
};
