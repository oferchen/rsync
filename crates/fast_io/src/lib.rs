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
//! - **Cached sorting** with Schwartzian transform
//!
//! # I/O Fallback Chain
//!
//! The crate selects the best available I/O mechanism at runtime, falling back
//! through increasingly portable options:
//!
//! 1. **io_uring** - Linux 5.6+, `io_uring` feature enabled, kernel not blocking
//!    the syscalls via seccomp. Batches multiple reads/writes into a single
//!    `io_uring_enter` syscall.
//! 2. **`copy_file_range`** - Linux 4.5+ for same-filesystem, 5.3+ for
//!    cross-filesystem. Zero-copy file-to-file transfer in kernel space.
//! 3. **`sendfile`** - Linux. Zero-copy file-to-socket transfer.
//! 4. **Standard buffered I/O** - All platforms. Uses `BufReader`/`BufWriter`
//!    with 64 KB default buffers.
//!
//! Each mechanism independently falls back to standard I/O on failure (e.g.,
//! NFS/FUSE mounts, old kernels, seccomp restrictions).
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

pub mod cached_sort;
pub mod parallel;
pub mod traits;

pub mod copy_file_range;
pub mod o_tmpfile;
pub mod sendfile;
pub mod syscall_batch;

#[cfg(unix)]
pub mod mmap_reader;
#[cfg(not(unix))]
#[path = "mmap_reader_stub.rs"]
pub mod mmap_reader;

/// io_uring-based async file and socket I/O for Linux 5.6+.
///
/// This module provides high-performance I/O using Linux's io_uring interface
/// with automatic fallback to standard buffered I/O on unsupported systems.
/// On non-Linux platforms or without the `io_uring` cargo feature, a stub
/// module is compiled that always returns standard I/O implementations.
///
/// See the module-level documentation for kernel requirements, runtime
/// detection, and the SQPOLL/fd-registration features.
#[cfg(all(target_os = "linux", feature = "io_uring"))]
pub mod io_uring;
#[cfg(not(all(target_os = "linux", feature = "io_uring")))]
#[path = "io_uring_stub.rs"]
pub mod io_uring;

pub use cached_sort::{CachedSortKey, cached_sort_by};
pub use parallel::{ParallelExecutor, ParallelResult};
pub use traits::{FileReader, FileWriter};

pub use mmap_reader::MmapReader;

pub use o_tmpfile::o_tmpfile_available;

pub use io_uring::{
    IoUringConfig, IoUringOrStdReader, IoUringOrStdWriter, IoUringReader, IoUringReaderFactory,
    IoUringWriter, IoUringWriterFactory, is_io_uring_available, reader_from_path, writer_from_file,
};

#[cfg(unix)]
pub use io_uring::{
    IoUringOrStdSocketReader, IoUringOrStdSocketWriter, IoUringSocketReader, IoUringSocketWriter,
    socket_reader_from_fd, socket_writer_from_fd,
};

/// Policy controlling io_uring usage for file and socket I/O.
///
/// This enum allows callers to explicitly enable, disable, or auto-detect
/// io_uring support. It is used by CLI flags `--io-uring` and `--no-io-uring`.
///
/// # Runtime detection
///
/// When set to `Auto`, the runtime check ([`io_uring::is_io_uring_available`])
/// performs three validations, caching the result in a process-wide atomic for
/// subsequent fast-path lookups:
///
/// 1. **Kernel version** - Parses `uname().release` and requires >= 5.6.
/// 2. **Syscall availability** - Attempts to create a minimal 4-entry io_uring
///    instance. This catches seccomp filters or container runtimes that block
///    `io_uring_setup(2)`.
/// 3. **Ring construction** - On first actual I/O, `IoUringConfig::build_ring`
///    creates the real ring. If SQPOLL is requested but the process lacks
///    `CAP_SYS_NICE`, it falls back to a normal ring silently.
///
/// If any step fails, the factory transparently returns a standard buffered
/// I/O reader or writer with no error.
///
/// # Kernel version requirements
///
/// | Feature | Minimum kernel | Notes |
/// |---------|---------------|-------|
/// | Basic io_uring (read/write) | 5.6 | `io_uring_setup`, `io_uring_enter` |
/// | `IORING_REGISTER_FILES` | 5.6 | Fixed-file descriptors, ~50ns/SQE savings |
/// | `IORING_SETUP_SQPOLL` | 5.6 | Kernel-side SQ polling, needs `CAP_SYS_NICE` |
/// | `IORING_OP_SEND` (socket I/O) | 5.6 | Used for socket writer batching |
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum IoUringPolicy {
    /// Auto-detect io_uring availability at runtime (default).
    ///
    /// Uses io_uring on Linux 5.6+ when the `io_uring` feature is enabled
    /// and the kernel supports it. Falls back to standard buffered I/O
    /// otherwise. This is the recommended setting for production use.
    #[default]
    Auto,
    /// Force io_uring usage. Returns an error if io_uring is unavailable.
    ///
    /// Useful for testing or when io_uring is required for performance
    /// guarantees. Fails with `ErrorKind::Unsupported` on non-Linux
    /// platforms, kernels older than 5.6, or when seccomp blocks the
    /// syscalls.
    Enabled,
    /// Disable io_uring; always use standard buffered I/O.
    ///
    /// Useful for benchmarking or diagnosing io_uring-related issues.
    Disabled,
}
