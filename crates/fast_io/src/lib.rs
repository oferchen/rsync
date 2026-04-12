//! High-performance I/O abstractions for rsync.
//!
//! This crate provides optimized I/O primitives that leverage modern OS features
//! and parallelism to maximize throughput.
//!
//! # Unsafe Boundary Isolation
//!
//! `fast_io` is the designated encapsulation layer for unsafe I/O optimizations.
//! Consumer crates - `engine` (`#[deny(unsafe_code)]`), `transfer`
//! (`#[deny(unsafe_code)]` per-module), `core`, and others - depend on `fast_io`
//! through safe public APIs only. This separation ensures that unsafe code is
//! confined to a single, auditable crate rather than scattered across the tree.
//!
//! ## Invariants that unsafe code in this crate must uphold
//!
//! - **Valid file descriptors** - every raw fd passed to FFI (`sendfile`,
//!   `splice`, `copy_file_range`, `utimensat`, io_uring submission) must be open and owned
//!   or borrowed for the duration of the call.
//! - **Proper lifetimes** - buffers handed to the kernel (io_uring SQEs, mmap
//!   regions) must outlive the I/O operation. No use-after-free on async completion.
//! - **No data races** - `unsafe impl Send` is only applied to fd-wrapper types
//!   whose descriptors are not aliased across threads.
//! - **Graceful fallback** - every unsafe optimization must have a safe fallback
//!   path so callers work on all platforms and filesystem types (NFS, FUSE, etc.).
//!
//! ## Adding new unsafe optimizations
//!
//! 1. Implement the optimization inside `fast_io` behind a safe public API.
//! 2. Gate platform-specific code with `#[cfg(...)]` and provide a stub module.
//! 3. Document the safety argument on every `unsafe` block - why each invariant holds.
//! 4. Add tests that exercise both the optimized and fallback paths.
//! 5. Never expose raw pointers, fds, or `unsafe fn` in the public API.
//!
//! # Features
//!
//! - **Parallel file operations** using rayon for multi-core utilization
//! - **Memory-mapped I/O** for large files with runtime fallback to buffered I/O
//! - **Zero-copy file transfer** using `copy_file_range` for file-to-file copies
//! - **Zero-copy socket send** using `sendfile` for file-to-socket transfers
//! - **Zero-copy socket receive** using `splice` for socket-to-file transfers (Linux)
//! - **Windows optimized copy** using `CopyFileExW` with optional no-buffering
//! - **ReFS reflink** via `FSCTL_DUPLICATE_EXTENTS_TO_FILE` for instant CoW on Windows
//! - **io_uring** for batched syscalls on Linux (optional, `io_uring` feature)
//! - **Platform copy trait** abstracting `copy_file_range`, `clonefile`, `CopyFileExW`
//! - **Cached sorting** with Schwartzian transform
//!
//! # I/O Fallback Chain
//!
//! The crate selects the best available I/O mechanism at runtime, falling back
//! through increasingly portable options:
//!
//! 1. **`FICLONE`** - Linux 4.5+, Btrfs/XFS/bcachefs. Instant copy-on-write
//!    reflink clone. O(1) regardless of file size.
//! 2. **io_uring** - Linux 5.6+, `io_uring` feature enabled, kernel not blocking
//!    the syscalls via seccomp. Batches multiple reads/writes into a single
//!    `io_uring_enter` syscall.
//! 3. **`copy_file_range`** - Linux 4.5+ for same-filesystem, 5.3+ for
//!    cross-filesystem. Zero-copy file-to-file transfer in kernel space.
//! 4. **`sendfile`** - Linux. Zero-copy file-to-socket transfer.
//! 5. **`splice`** - Linux 2.6.17+. Zero-copy socket-to-file transfer via
//!    pipe intermediary. Used for network receive paths.
//! 6. **Standard buffered I/O** - All platforms. Uses `BufReader`/`BufWriter`
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

pub mod copy_file_ex;
pub mod copy_file_range;
pub mod o_tmpfile;
pub mod platform_copy;
pub mod refs_detect;
pub mod sendfile;
pub mod splice;
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
pub use platform_copy::{
    CopyMethod, CopyResult, DefaultPlatformCopy, PlatformCopy, try_clonefile, try_fcopyfile,
    try_ficlone, try_refs_reflink,
};
pub use traits::{FileReader, FileWriter};

pub use refs_detect::{clear_refs_cache, is_refs_filesystem};
pub use splice::{is_splice_available, try_splice_to_file};

pub use mmap_reader::MmapReader;
pub use o_tmpfile::{
    AnonymousTempFile, OTmpfileSupport, TempFileResult, link_anonymous_tmpfile,
    o_tmpfile_available, o_tmpfile_probe, open_anonymous_tmpfile, open_temp_file,
};

pub use io_uring::{
    IoUringConfig, IoUringDiskBatch, IoUringOrStdReader, IoUringOrStdWriter, IoUringReader,
    IoUringReaderFactory, IoUringWriter, IoUringWriterFactory, RegisteredBufferGroup,
    RegisteredBufferSlot, is_io_uring_available, reader_from_path, writer_from_file,
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
