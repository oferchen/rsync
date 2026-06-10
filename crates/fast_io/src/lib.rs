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
//! - **macOS event loop** using `kqueue` / `kevent` for readiness-driven I/O (#1385)
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

/// Cached sorting using the Schwartzian transform.
pub mod cached_sort;
/// Rootless container / user-namespace detection for SQPOLL gating.
pub mod container;
/// Parent-dirfd carrier (`DirSandbox`) for the SEC-1 sandbox.
///
/// Threads an in-tree dirfd stack plus a `DashMap<PathBuf, Arc<OwnedFd>>`
/// side cache through the receiver pipeline so SEC-1.f-j can convert
/// path-based syscalls to `*at` siblings without re-walking the path
/// through the kernel.
///
/// Unix-only: Windows callers use NTFS handle-based APIs which sidestep
/// path TOCTOU naturally (see the SEC-1.l audit).
#[cfg(unix)]
pub mod dir_sandbox;
/// Kernel version parsing and io_uring probe logging.
pub mod kernel_version;
/// Cached runtime probes for Linux-specific kernel capabilities used by the
/// SEC-1 dirfd sandbox.
///
/// Unix-only: on non-Linux Unix targets the helpers short-circuit to
/// compile-time `false`. Windows callers use NTFS handle-based APIs (see
/// the SEC-1.l audit) and do not depend on this module.
#[cfg(unix)]
pub mod linux_capabilities;
/// Page-aligned buffer pool for IOCP no-buffering mode.
pub mod page_aligned;
/// Parallel file I/O operations using rayon.
pub mod parallel;
/// Strict-resolution directory open for the SEC-1 dirfd sandbox.
///
/// Unix-only: Windows callers use NTFS handle-based APIs which sidestep
/// path TOCTOU naturally (see the SEC-1.l audit).
#[cfg(unix)]
pub mod secure_dir;
/// Cross-platform temporary file strategy abstraction.
pub mod temp_file_strategy;
/// Core traits for file I/O abstraction.
pub mod traits;
/// SIMD-accelerated zero-byte detection for sparse file writing.
pub mod zero_detect;

/// EXPERIMENTAL: per-file adaptive basis-read backend dispatch (SMR-3c,
/// Option 3 from `docs/design/mmap-vs-sqpoll-conflict-resolution.md`).
///
/// Gated by the `adaptive-basis-dispatch` Cargo feature, which is **not**
/// enabled by default. When the feature is off this module is not
/// compiled and the live dispatch path is byte-identical to today.
#[cfg(feature = "adaptive-basis-dispatch")]
pub mod adaptive_dispatch;

/// Offset-aware basis-to-destination range copy via `copy_file_range(2)` for
/// the delta-apply COPY-token fast path (IUD-10).
pub mod copy_basis_range;
/// Windows `CopyFileExW` file copy with automatic fallback.
pub mod copy_file_ex;
/// High-performance file copying with tiered fallback.
pub mod copy_file_range;
/// Anonymous temporary file creation via `O_TMPFILE` and finalization via `linkat`.
pub mod o_tmpfile;
/// Platform-abstracted file copy trait with automatic optimization selection.
pub mod platform_copy;
/// ReFS filesystem detection for Windows reflink support.
pub mod refs_detect;
/// Zero-copy file-to-socket transfer using `sendfile` with automatic fallback.
pub mod sendfile;
/// Safe wrappers around platform signal-handler installation.
pub mod signal;
/// Safe wrappers around platform `setsockopt` for integer-valued options.
pub mod socket_options;
/// Zero-copy socket-to-disk transfer using `splice`/`vmsplice` syscalls.
pub mod splice;
/// Batched metadata syscall operations with dual-path runtime selection.
pub mod syscall_batch;
/// Zero-copy file writer that pushes literal chunks via `vmsplice` + `splice`.
pub mod vmsplice_writer;
/// Delete-on-close temporary file creation for Windows via `FileDispositionInfo`.
pub mod win_tmpfile;

/// macOS-optimized file writer using `F_NOCACHE` and `writev`.
pub mod macos_io;

/// Memory-mapped file reader for efficient large file access.
#[cfg(unix)]
pub mod mmap_reader;
/// Memory-mapped file reader stub for non-Unix platforms.
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
#[path = "io_uring_stub/mod.rs"]
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
#[path = "iocp_stub/mod.rs"]
pub mod iocp;

/// Landlock LSM defense-in-depth allowlist for the daemon receiver path
/// (SEC-1.p).
///
/// On Linux with the `landlock` feature, exposes `restrict_to_module_paths`
/// which engages a kernel-enforced allowlist above the SEC-1 `*at` helpers.
/// On every other target (or with the feature disabled) a stub module
/// compiles with the same public surface: every call returns `Unavailable`
/// so cross-platform callers share a single code path. See
/// `docs/design/sec-1-p-landlock-defense-in-depth-2026-05-22.md` for the
/// kernel-version matrix and integration plan.
#[cfg(all(target_os = "linux", feature = "landlock"))]
pub mod landlock;
#[cfg(not(all(target_os = "linux", feature = "landlock")))]
#[path = "landlock_stub.rs"]
pub mod landlock;

/// macOS `kqueue`-based event loop primitive.
///
/// Exposes a thin safe wrapper over `kqueue(2)` / `kevent(2)` used as
/// the foundation for the kqueue-driven `AsyncFileWriter` backend
/// (#1385). On macOS the real implementation is compiled in. On every
/// other platform a stub module provides the same public API with
/// constructors returning `io::ErrorKind::Unsupported`, so callers can
/// probe availability at runtime without `#[cfg]` branching.
///
/// See `docs/design/macos-kqueue-fast-io.md` for design rationale and
/// the planned consumer migrations (disk-commit thread, daemon accept
/// loop).
#[cfg(target_os = "macos")]
pub mod kqueue;
#[cfg(not(target_os = "macos"))]
#[path = "kqueue_stub.rs"]
pub mod kqueue;

mod io_uring_common;
mod io_uring_depth;
mod io_uring_ops;
mod policy;
/// SQM-3: pin mmap'd basis windows in memory before SQPOLL submissions
/// so the kthread cannot fault on a page the userspace task has not
/// touched. See `docs/design/sqm-2b-implementation-design.md`.
pub mod sqpoll_basis;
mod status;

pub use cached_sort::{CachedSortKey, cached_sort_by};
pub use container::detect_rootless_container;
pub use copy_basis_range::{
    COPY_BASIS_RANGE_MIN_BYTES, copy_basis_range, copy_file_range_supported,
};
pub use page_aligned::{PageAlignedBuffer, page_size, round_up_to_page};
pub use parallel::{ParallelExecutor, ParallelResult};
pub use platform_copy::{
    CopyMethod, CopyResult, DefaultPlatformCopy, NoCowPlatformCopy, NoZeroCopyPlatformCopy,
    PlatformCopy, try_clonefile, try_fcopyfile, try_ficlone, try_refs_reflink,
    try_refs_reflink_range,
};
pub use traits::{FileReader, FileWriter};

#[cfg(unix)]
pub use dir_sandbox::{
    AtMetadata, DirEntryView, DirSandbox, EntryKind, LstatOutcome, ReadDirOutcome, UnlinkFlags,
    fchmodat, fchmodat_via_sandbox_or_fallback, fchownat, fchownat_via_sandbox_or_fallback,
    fstatat_nofollow, linkat, linkat_via_sandbox_or_fallback, lstat_via_sandbox_or_fallback,
    mkdirat, mkdirat_via_sandbox_or_fallback, openat, openat_via_sandbox_or_fallback,
    read_dir_via_sandbox_or_fallback, readlinkat, readlinkat_via_sandbox_or_fallback,
    recursive_unlinkat, recursive_unlinkat_via_sandbox_or_fallback, renameat,
    renameat_via_sandbox_or_fallback, symlinkat, symlinkat_via_sandbox_or_fallback,
    unlink_via_sandbox_or_fallback, unlinkat, utimensat, utimensat_via_sandbox_or_fallback,
};
pub use kernel_version::{
    IO_URING_MIN_KERNEL, IoUringRequirement, KernelVersion, LinkatRequirement, PbufRingRequirement,
    SendZcRequirement, StatxRenameatRequirement, VersionRequirement, log_io_uring_probe_result,
    parse_kernel_version,
};
#[cfg(unix)]
pub use linux_capabilities::openat2_supported;
pub use refs_detect::{clear_refs_cache, is_refs_filesystem};
#[cfg(unix)]
pub use secure_dir::secure_open_dir;
pub use sendfile::send_file_to_fd_with_policy;
pub use socket_options::set_socket_int_option;
#[cfg(target_os = "linux")]
pub use splice::DEFAULT_PIPE_CAPACITY;
pub use splice::{
    SplicePipe, is_splice_available, is_splice_enabled, recv_fd_to_file, try_splice_to_file,
    try_vmsplice_to_file,
};
pub use vmsplice_writer::{VMSPLICE_MIN_CHUNK, VmspliceFileWriter};

pub use macos_io::{
    F_NOCACHE_THRESHOLD, MAX_IOV_COUNT, MacosWriter, apply_sequential_read_hint, is_nocache_set,
    set_nocache, writev_buffers,
};

pub use kqueue::{KEvent, KEventFilter, KqueueLoop, is_kqueue_available};

pub use mmap_reader::MmapReader;
pub use o_tmpfile::{
    AnonymousTempFile, OTmpfileSupport, TempFileResult, link_anonymous_tmpfile,
    o_tmpfile_available, o_tmpfile_probe, open_anonymous_tmpfile, open_temp_file,
};
#[cfg(target_os = "linux")]
pub use temp_file_strategy::AnonymousTempFileStrategy;
#[cfg(target_os = "windows")]
pub use temp_file_strategy::WindowsTempFileStrategy;
pub use temp_file_strategy::{
    DefaultTempFileStrategy, NamedTempFileStrategy, TempFileHandle, TempFileKind, TempFileStrategy,
};
pub use win_tmpfile::{
    WinDeleteOnCloseSupport, WinTempFileResult, WindowsTempFile, clear_delete_on_close,
    commit_delete_on_close, delete_on_close_available, open_delete_on_close_tmpfile,
    open_win_temp_file, rename_temp_to_dest, set_delete_on_close, win_tmpfile_probe,
};

pub use io_uring::{
    BgidAllocError, BgidAllocator, BgidSessionStats, BgidSnapshot, BufferRing, BufferRingConfig,
    BufferRingError, CancelOutcome, IORING_OP_LINKAT, IORING_OP_RENAMEAT, IORING_OP_STATX,
    IoUringConfig, IoUringDiskBatch, IoUringKernelInfo, IoUringOrStdReader, IoUringOrStdWriter,
    IoUringReader, IoUringReaderFactory, IoUringWriter, IoUringWriterFactory, LINKAT_MIN_KERNEL,
    LinkAtArgs, OpTag, RENAME_EXCHANGE, RENAME_NOREPLACE, RENAME_WHITEOUT, RegisteredBufferGroup,
    RegisteredBufferSlot, RegisteredBufferStats, RegisteredBufferStatus, RenameAt2Args, RingLease,
    STATX_MIN_KERNEL, SessionPoolConfig, SessionRingPool, SharedCompletion, SharedRing,
    SharedRingConfig, StatxArgs, StatxResult, ThreadLocalRingLease, ThreadLocalRingPool,
    bgid_exhausted_count, bgid_inflight, bgid_peak_used, bgid_snapshot, buffer_id_from_cqe_flags,
    build_linkat_sqe, build_linkat_sqe_unchecked, build_renameat2_sqe,
    build_renameat2_sqe_unchecked, build_statx_sqe, build_statx_sqe_unchecked,
    is_io_uring_available, is_sqpoll_disabled_by_policy, linkat_supported, pbuf_ring_supported,
    reader_from_path, reader_from_path_with_depth, renameat2_blocking, renameat2_supported,
    set_sqpoll_disabled_by_policy, sqpoll_fell_back, statx_supported, submit_linkat_blocking,
    submit_statx_batch, submit_statx_blocking, writer_from_file, writer_from_file_with_depth,
};

#[cfg(all(target_os = "linux", feature = "iouring-data-reads"))]
pub use io_uring::IoUringFileReader;

/// Reads `path` in full via the IUD-6 io_uring slurp wrapper.
///
/// Opens the file with `IoUringFileReader::open` and pulls the entire
/// contents through the registered-buffer `READ_FIXED` pipeline shared with
/// the main `IoUringReader`. Intended for opt-in dispatch on basis-file
/// reads above a callsite-defined size threshold; default builds neither
/// compile this entry point nor call it.
///
/// # Errors
///
/// Returns the underlying io_uring error if ring construction, submission,
/// or completion processing fails.
#[cfg(all(target_os = "linux", feature = "iouring-data-reads"))]
pub fn read_file_with_io_uring<P: AsRef<std::path::Path>>(path: P) -> std::io::Result<Vec<u8>> {
    IoUringFileReader::open(path.as_ref())?.read_to_end()
}

/// Stub for non-Linux targets or when the `iouring-data-reads` feature is
/// disabled. Always returns [`std::io::ErrorKind::Unsupported`] so callers
/// fall back to their default reader.
#[cfg(not(all(target_os = "linux", feature = "iouring-data-reads")))]
pub fn read_file_with_io_uring<P: AsRef<std::path::Path>>(_path: P) -> std::io::Result<Vec<u8>> {
    Err(std::io::Error::new(
        std::io::ErrorKind::Unsupported,
        "iouring-data-reads feature is not enabled on this build",
    ))
}

#[cfg(all(target_os = "windows", feature = "iocp"))]
pub use iocp::post_completion as iocp_post_completion;
pub use iocp::{
    CompletionHandler, CompletionPump, IocpConfig, IocpDiskBatch, IocpError, IocpOrStdReader,
    IocpOrStdWriter, IocpPumpConfig, IocpReader, IocpReaderFactory, IocpWriter, IocpWriterFactory,
    bounce_copies_avoided as iocp_bounce_copies_avoided, iocp_availability_reason,
    is_iocp_available, oneshot_handler, skip_event_optimization_available,
};
#[cfg(all(target_os = "windows", feature = "transmitfile"))]
pub use iocp::{TRANSMIT_FILE_MAX_BYTES, try_transmit_file};
pub use iocp::{
    reader_from_path as iocp_reader_from_path, writer_from_file as iocp_writer_from_file,
};

#[cfg(unix)]
pub use io_uring::{
    IoUringOrStdSocketReader, IoUringOrStdSocketWriter, IoUringSocketReader, IoUringSocketWriter,
    socket_reader_from_fd, socket_writer_from_fd,
};

/// Opt-in `IORING_OP_SEND_ZC` transport-send dispatch (Linux + `io_uring`).
///
/// Exposed only when the `iouring-send-zc` cargo feature is enabled. The
/// stub on non-Linux returns [`std::io::ErrorKind::Unsupported`] from every
/// method so cross-platform callers compile but never route real traffic
/// through the zero-copy path. See the module docs on `io_uring::send_zc`
/// for the buffer-lifetime contract.
#[cfg(feature = "iouring-send-zc")]
pub use io_uring::{SEND_ZC_DISPATCH_MIN_BYTES, ZeroCopySender};

/// Writes `data` to `path` via the io_uring registered-buffer write path.
///
/// When compiled with both `target_os = "linux"` and the
/// `iouring-data-writes` feature, this dispatches to the live
/// [`io_uring::write_file_with_io_uring`] helper which reuses
/// [`io_uring::IoUringWriter`] and the registered-buffer pool. On every other
/// configuration, the function returns `io::ErrorKind::Unsupported` so
/// callers can fall back to their existing copy path.
///
/// # Errors
///
/// Returns `io::ErrorKind::Unsupported` when the platform or feature gate
/// disables the io_uring data-write path. On Linux with the feature on,
/// propagates ring-construction, submission, and fsync errors from the
/// underlying writer.
#[cfg(all(target_os = "linux", feature = "iouring-data-writes"))]
pub fn write_file_with_io_uring(path: &std::path::Path, data: &[u8]) -> std::io::Result<()> {
    io_uring::write_file_with_io_uring(path, data)
}

/// Stub for non-Linux targets or when `iouring-data-writes` is disabled.
///
/// Always returns `io::ErrorKind::Unsupported`. Callers should treat this
/// outcome as the signal to use their standard write path.
#[cfg(not(all(target_os = "linux", feature = "iouring-data-writes")))]
pub fn write_file_with_io_uring(_path: &std::path::Path, _data: &[u8]) -> std::io::Result<()> {
    Err(std::io::Error::new(
        std::io::ErrorKind::Unsupported,
        "iouring-data-writes feature is not enabled for this build",
    ))
}

pub use io_uring_common::IoBackend;
pub use io_uring_depth::{
    IO_URING_DEPTH_MAX, IO_URING_DEPTH_MIN, IoUringDepthError, validate_io_uring_depth,
};
pub use io_uring_ops::{
    hard_link, try_hard_link_via_io_uring, try_rename_via_io_uring, try_statx_batch_via_io_uring,
};
pub use policy::{
    BackendPolicy, BasisReadBackend, CowPolicy, IoUringPolicy, IocpPolicy,
    MMAP_TO_SQPOLL_THRESHOLD, MMAP_TO_SQPOLL_THRESHOLD_ENV, ZeroCopyPolicy,
    choose_basis_read_backend, choose_basis_read_backend_with_threshold,
    mmap_to_sqpoll_threshold_bytes,
};
pub use sqpoll_basis::{
    MAX_WIRED_WINDOW_BYTES, MlockError, WiredBasisWindow, mlock_attempts, mlock_downgrades,
};
pub use status::{
    IoUringRestriction, detect_io_uring_restriction, io_uring_availability_reason,
    io_uring_capability_matrix, io_uring_kernel_info, io_uring_status_detail, iocp_status_detail,
    platform_io_capabilities,
};
