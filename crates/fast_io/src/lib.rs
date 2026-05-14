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
//! - **Windows IOCP** for overlapped async file I/O (optional, `iocp` feature)
//! - **io_uring** for batched syscalls on Linux (optional, `io_uring` feature)
//! - **macOS optimized writes** using `F_NOCACHE` (cache bypass) and `writev` (scatter-gather)
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
pub mod kernel_version;
pub mod parallel;
pub mod temp_file_strategy;
pub mod traits;
pub mod zero_detect;

pub mod copy_file_ex;
pub mod copy_file_range;
pub mod o_tmpfile;
pub mod platform_copy;
pub mod refs_detect;
pub mod sendfile;
pub mod socket_options;
pub mod splice;
pub mod syscall_batch;

pub mod macos_io;

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

/// Windows I/O Completion Ports (IOCP) for async file I/O.
///
/// This module provides high-performance overlapped file I/O using Windows
/// IOCP with automatic fallback to standard buffered I/O on unsupported
/// systems. On non-Windows platforms or without the `iocp` cargo feature,
/// a stub module is compiled that always returns standard I/O implementations.
#[cfg(all(target_os = "windows", feature = "iocp"))]
pub mod iocp;
#[cfg(not(all(target_os = "windows", feature = "iocp")))]
#[path = "iocp_stub.rs"]
pub mod iocp;

mod io_uring_depth;
mod io_uring_ops;
mod policy;
mod status;

pub use cached_sort::{CachedSortKey, cached_sort_by};
pub use parallel::{ParallelExecutor, ParallelResult};
pub use platform_copy::{
    CopyMethod, CopyResult, DefaultPlatformCopy, NoCowPlatformCopy, NoZeroCopyPlatformCopy,
    PlatformCopy, try_clonefile, try_fcopyfile, try_ficlone, try_refs_reflink,
    try_refs_reflink_range,
};
pub use traits::{FileReader, FileWriter};

pub use kernel_version::{
    IO_URING_MIN_KERNEL, KernelVersion, log_io_uring_probe_result, parse_kernel_version,
};
pub use refs_detect::{clear_refs_cache, is_refs_filesystem};
pub use sendfile::send_file_to_fd_with_policy;
pub use socket_options::set_socket_int_option;
#[cfg(target_os = "linux")]
pub use splice::DEFAULT_PIPE_CAPACITY;
pub use splice::{
    SplicePipe, is_splice_available, is_splice_enabled, recv_fd_to_file, try_splice_to_file,
    try_vmsplice_to_file,
};

pub use macos_io::{
    F_NOCACHE_THRESHOLD, MAX_IOV_COUNT, MacosWriter, is_nocache_set, set_nocache, writev_buffers,
};

pub use mmap_reader::MmapReader;
pub use o_tmpfile::{
    AnonymousTempFile, OTmpfileSupport, TempFileResult, link_anonymous_tmpfile,
    o_tmpfile_available, o_tmpfile_probe, open_anonymous_tmpfile, open_temp_file,
};
#[cfg(target_os = "linux")]
pub use temp_file_strategy::AnonymousTempFileStrategy;
pub use temp_file_strategy::{
    DefaultTempFileStrategy, NamedTempFileStrategy, TempFileHandle, TempFileKind, TempFileStrategy,
};

pub use io_uring::{
    BgidAllocator, BufferRing, BufferRingConfig, BufferRingError, IORING_OP_LINKAT,
    IORING_OP_RENAMEAT, IORING_OP_STATX, IoUringConfig, IoUringDiskBatch, IoUringKernelInfo,
    IoUringOrStdReader, IoUringOrStdWriter, IoUringReader, IoUringReaderFactory, IoUringWriter,
    IoUringWriterFactory, LINKAT_MIN_KERNEL, LinkAtArgs, OpTag, RENAME_EXCHANGE, RENAME_NOREPLACE,
    RENAME_WHITEOUT, RegisteredBufferGroup, RegisteredBufferSlot, RegisteredBufferStats,
    RegisteredBufferStatus, RenameAt2Args, STATX_MIN_KERNEL, SharedCompletion, SharedRing,
    SharedRingConfig, StatxArgs, StatxResult, buffer_id_from_cqe_flags, build_linkat_sqe,
    build_linkat_sqe_unchecked, build_renameat2_sqe, build_renameat2_sqe_unchecked,
    build_statx_sqe, build_statx_sqe_unchecked, is_io_uring_available, linkat_supported,
    pbuf_ring_supported, reader_from_path, reader_from_path_with_depth, renameat2_blocking,
    renameat2_supported, sqpoll_fell_back, statx_supported, submit_linkat_blocking,
    submit_statx_batch, submit_statx_blocking, writer_from_file, writer_from_file_with_depth,
};

#[cfg(all(target_os = "windows", feature = "iocp"))]
pub use iocp::post_completion as iocp_post_completion;
pub use iocp::{
    CompletionHandler, CompletionPump, IocpConfig, IocpDiskBatch, IocpError, IocpOrStdReader,
    IocpOrStdWriter, IocpPumpConfig, IocpReader, IocpReaderFactory, IocpWriter, IocpWriterFactory,
    iocp_availability_reason, is_iocp_available, oneshot_handler,
    skip_event_optimization_available,
};
pub use iocp::{
    reader_from_path as iocp_reader_from_path, writer_from_file as iocp_writer_from_file,
};

#[cfg(unix)]
pub use io_uring::{
    IoUringOrStdSocketReader, IoUringOrStdSocketWriter, IoUringSocketReader, IoUringSocketWriter,
    socket_reader_from_fd, socket_writer_from_fd,
};

pub use io_uring_depth::{
    IO_URING_DEPTH_MAX, IO_URING_DEPTH_MIN, IoUringDepthError, validate_io_uring_depth,
};
pub use io_uring_ops::{
    hard_link, try_hard_link_via_io_uring, try_rename_via_io_uring, try_statx_batch_via_io_uring,
};
pub use policy::{CowPolicy, IoUringPolicy, IocpPolicy, ZeroCopyPolicy};
pub use status::{
    io_uring_availability_reason, io_uring_kernel_info, io_uring_status_detail, iocp_status_detail,
    platform_io_capabilities,
};
