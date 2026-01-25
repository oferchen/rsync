//! High-performance I/O abstractions for rsync.
//!
//! This crate provides optimized I/O primitives that leverage modern OS features
//! and parallelism to maximize throughput.
//!
//! # Features
//!
//! - **Parallel file operations** using rayon for multi-core utilization
//! - **Memory-mapped I/O** for large files (optional, `mmap` feature)
//! - **io_uring** for batched syscalls on Linux (optional, `io_uring` feature)
//! - **Buffer pools** for reduced allocation overhead
//! - **Cached sorting** with Schwartzian transform
//!
//! # Design Principles
//!
//! 1. **Zero-copy where possible** - Use mmap and buffer reuse
//! 2. **Batch operations** - Reduce syscall overhead
//! 3. **Parallel by default** - Utilize all CPU cores
//! 4. **Graceful fallback** - Work on all platforms

#![deny(rustdoc::broken_intra_doc_links)]
#![deny(missing_docs)]

pub mod buffer_pool;
pub mod cached_sort;
#[cfg(feature = "mmap")]
pub mod mmap_reader;
pub mod parallel;
pub mod traits;

pub use buffer_pool::{BufferGuard, BufferPool};
pub use cached_sort::{cached_sort_by, CachedSortKey};
pub use parallel::{ParallelExecutor, ParallelResult};
pub use traits::{FileReader, FileWriter};

#[cfg(feature = "mmap")]
pub use mmap_reader::MmapReader;
