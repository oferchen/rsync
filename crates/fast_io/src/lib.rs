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
    RenameAt2Args, STATX_MIN_KERNEL, SharedCompletion, SharedRing, SharedRingConfig, StatxArgs,
    StatxResult, buffer_id_from_cqe_flags, build_linkat_sqe, build_linkat_sqe_unchecked,
    build_renameat2_sqe, build_renameat2_sqe_unchecked, build_statx_sqe, build_statx_sqe_unchecked,
    is_io_uring_available, linkat_supported, pbuf_ring_supported, reader_from_path,
    reader_from_path_with_depth, renameat2_blocking, renameat2_supported, sqpoll_fell_back,
    statx_supported, submit_linkat_blocking, submit_statx_batch, submit_statx_blocking,
    writer_from_file, writer_from_file_with_depth,
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

/// Detailed IOCP availability status for `--version` output.
///
/// Returns a human-readable string describing IOCP support:
/// - Whether the feature was compiled in
/// - Whether the OS supports it (Windows only)
#[must_use]
pub fn iocp_status_detail() -> String {
    iocp_status_detail_impl()
}

#[cfg(all(target_os = "windows", feature = "iocp"))]
fn iocp_status_detail_impl() -> String {
    if is_iocp_available() {
        let skip_event = if skip_event_optimization_available() {
            ", FILE_SKIP_SET_EVENT_ON_HANDLE active"
        } else {
            ""
        };
        format!("compiled in, available{skip_event}")
    } else {
        "compiled in, unavailable (CreateIoCompletionPort failed)".to_string()
    }
}

#[cfg(not(all(target_os = "windows", feature = "iocp")))]
fn iocp_status_detail_impl() -> String {
    #[cfg(not(target_os = "windows"))]
    {
        "not available (platform is not Windows)".to_string()
    }
    #[cfg(all(target_os = "windows", not(feature = "iocp")))]
    {
        "not compiled in (iocp feature disabled)".to_string()
    }
}

/// Detailed io_uring availability status for `--version` output.
///
/// Returns a human-readable string describing io_uring support:
/// - Whether the feature was compiled in
/// - Whether the kernel supports it (Linux only)
/// - The detected kernel version when relevant
#[must_use]
pub fn io_uring_status_detail() -> String {
    io_uring_status_detail_impl()
}

/// Returns a log-friendly reason string for io_uring availability.
///
/// On Linux with the `io_uring` feature enabled, probes the kernel version
/// and attempts `io_uring_setup(2)`, returning a message like:
/// - `"io_uring: enabled (kernel 5.15, 48 ops supported)"`
/// - `"io_uring: disabled (kernel 4.19 < 5.6 required)"`
/// - `"io_uring: disabled (kernel 6.1, io_uring_setup(2) blocked by seccomp, container, or permission restriction)"`
///
/// On non-Linux platforms or without the feature, returns a compile-time reason.
#[must_use]
pub fn io_uring_availability_reason() -> String {
    io_uring_availability_reason_impl()
}

/// Returns structured kernel information for io_uring availability.
///
/// Provides machine-readable fields for callers that need to act on
/// kernel version or supported op count programmatically. On non-Linux
/// platforms or without the `io_uring` feature, returns a struct with
/// `available: false` and `None` kernel versions.
#[must_use]
pub fn io_uring_kernel_info() -> io_uring::IoUringKernelInfo {
    io_uring_kernel_info_impl()
}

#[cfg(all(target_os = "linux", feature = "io_uring"))]
fn io_uring_availability_reason_impl() -> String {
    io_uring::config_detail::io_uring_availability_reason()
}

#[cfg(not(all(target_os = "linux", feature = "io_uring")))]
fn io_uring_availability_reason_impl() -> String {
    #[cfg(not(target_os = "linux"))]
    {
        "io_uring: disabled (platform is not Linux)".to_string()
    }
    #[cfg(all(target_os = "linux", not(feature = "io_uring")))]
    {
        "io_uring: disabled (io_uring feature not compiled in)".to_string()
    }
}

#[cfg(all(target_os = "linux", feature = "io_uring"))]
fn io_uring_kernel_info_impl() -> io_uring::IoUringKernelInfo {
    io_uring::config_detail::io_uring_kernel_info()
}

#[cfg(not(all(target_os = "linux", feature = "io_uring")))]
fn io_uring_kernel_info_impl() -> io_uring::IoUringKernelInfo {
    io_uring::IoUringKernelInfo {
        available: false,
        kernel_major: None,
        kernel_minor: None,
        supported_ops: 0,
        pbuf_ring_supported: false,
        reason: io_uring_availability_reason_impl(),
    }
}

#[cfg(all(target_os = "linux", feature = "io_uring"))]
fn io_uring_status_detail_impl() -> String {
    let info = io_uring::config_detail::io_uring_kernel_info();

    match (info.kernel_major, info.kernel_minor) {
        (Some(major), Some(minor)) => {
            if info.available {
                format!(
                    "compiled in, available (kernel {major}.{minor}, {} ops)",
                    info.supported_ops
                )
            } else {
                format!("compiled in, unavailable (kernel {major}.{minor}, requires >= 5.6)")
            }
        }
        _ => "compiled in, unavailable (could not detect kernel version)".to_string(),
    }
}

#[cfg(not(all(target_os = "linux", feature = "io_uring")))]
fn io_uring_status_detail_impl() -> String {
    #[cfg(not(target_os = "linux"))]
    {
        "not available (platform is not Linux)".to_string()
    }
    #[cfg(all(target_os = "linux", not(feature = "io_uring")))]
    {
        "not compiled in (io_uring feature disabled)".to_string()
    }
}

#[cfg(unix)]
pub use io_uring::{
    IoUringOrStdSocketReader, IoUringOrStdSocketWriter, IoUringSocketReader, IoUringSocketWriter,
    socket_reader_from_fd, socket_writer_from_fd,
};

/// Returns the platform I/O capabilities available on this system.
///
/// Each entry is a human-readable label describing an available fast I/O path.
/// Compile-time capabilities (determined by target OS) are always included when
/// applicable. Runtime-probed capabilities (io_uring, splice) are included only
/// when the probe succeeds.
///
/// # Platform-specific entries
///
/// - **Linux**: `copy_file_range`, `sendfile`, `splice` (runtime-probed),
///   `FICLONE`, `O_TMPFILE`, `io_uring` (runtime-probed)
/// - **macOS**: `clonefile`, `fcopyfile`, `F_NOCACHE`, `writev`
/// - **Windows**: `CopyFileEx`, `ReFS reflink`, `IOCP` (runtime-probed)
#[must_use]
pub fn platform_io_capabilities() -> Vec<&'static str> {
    let mut caps = Vec::new();

    // Linux compile-time capabilities
    #[cfg(target_os = "linux")]
    {
        caps.push("copy_file_range");
        caps.push("sendfile");

        if is_splice_available() {
            caps.push("splice");
        }

        caps.push("FICLONE");
        caps.push("O_TMPFILE");

        if is_io_uring_available() {
            caps.push("io_uring");
        }
    }

    // macOS compile-time capabilities
    #[cfg(target_os = "macos")]
    {
        caps.push("clonefile");
        caps.push("fcopyfile");
        caps.push("F_NOCACHE");
        caps.push("writev");
    }

    // Windows compile-time capabilities
    #[cfg(target_os = "windows")]
    {
        caps.push("CopyFileEx");
        caps.push("ReFS reflink");
        if is_iocp_available() {
            caps.push("IOCP");
        }
    }

    caps
}

/// Attempts to rename a file via io_uring `IORING_OP_RENAMEAT`.
///
/// On Linux with kernel 5.11+ and io_uring available, submits a blocking
/// RENAMEAT2 SQE on a transient ring and returns the result. On all other
/// platforms, or when the kernel lacks the opcode, returns `None` so the
/// caller can fall back to `std::fs::rename`.
///
/// This follows the same try-or-fallback pattern used by the splice and
/// copy-file-range paths: the caller checks the `Option` and falls through
/// to the portable implementation when `None` is returned.
///
/// # Errors
///
/// Returns `Some(Err(...))` when io_uring is available and the rename was
/// submitted but the kernel returned an error (e.g., `ENOENT`, `EACCES`).
pub fn try_rename_via_io_uring(
    old_path: &std::path::Path,
    new_path: &std::path::Path,
) -> Option<std::io::Result<()>> {
    try_rename_via_io_uring_impl(old_path, new_path)
}

#[cfg(all(target_os = "linux", feature = "io_uring"))]
fn try_rename_via_io_uring_impl(
    old_path: &std::path::Path,
    new_path: &std::path::Path,
) -> Option<std::io::Result<()>> {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;

    if !renameat2_supported() {
        return None;
    }
    let old_c = match CString::new(old_path.as_os_str().as_bytes()) {
        Ok(c) => c,
        Err(_) => return Some(Err(std::io::Error::other("path contains interior NUL"))),
    };
    let new_c = match CString::new(new_path.as_os_str().as_bytes()) {
        Ok(c) => c,
        Err(_) => return Some(Err(std::io::Error::other("path contains interior NUL"))),
    };
    let args = RenameAt2Args {
        old_dir_fd: libc::AT_FDCWD,
        old_path: &old_c,
        new_dir_fd: libc::AT_FDCWD,
        new_path: &new_c,
        flags: 0,
    };
    match renameat2_blocking(args) {
        Ok(result) if result < 0 => Some(Err(std::io::Error::from_raw_os_error(-result))),
        Ok(_) => Some(Ok(())),
        Err(e) if e.kind() == std::io::ErrorKind::Unsupported => None,
        Err(e) => Some(Err(e)),
    }
}

#[cfg(not(all(target_os = "linux", feature = "io_uring")))]
fn try_rename_via_io_uring_impl(
    _old_path: &std::path::Path,
    _new_path: &std::path::Path,
) -> Option<std::io::Result<()>> {
    None
}

/// Attempts to create a hard link via io_uring `IORING_OP_LINKAT`.
///
/// On Linux with kernel 5.15+ and io_uring available, submits a blocking
/// LINKAT SQE on a transient ring and returns the result. On all other
/// platforms, or when the kernel lacks the opcode, returns `None` so the
/// caller can fall back to `std::fs::hard_link`.
///
/// # Errors
///
/// Returns `Some(Err(...))` when io_uring is available and the link was
/// submitted but the kernel returned an error (e.g., `EEXIST`, `EACCES`).
pub fn try_hard_link_via_io_uring(
    src_path: &std::path::Path,
    dst_path: &std::path::Path,
) -> Option<std::io::Result<()>> {
    try_hard_link_via_io_uring_impl(src_path, dst_path)
}

#[cfg(all(target_os = "linux", feature = "io_uring"))]
fn try_hard_link_via_io_uring_impl(
    src_path: &std::path::Path,
    dst_path: &std::path::Path,
) -> Option<std::io::Result<()>> {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;

    if !linkat_supported() {
        return None;
    }
    let old_c = match CString::new(src_path.as_os_str().as_bytes()) {
        Ok(c) => c,
        Err(_) => return Some(Err(std::io::Error::other("path contains interior NUL"))),
    };
    let new_c = match CString::new(dst_path.as_os_str().as_bytes()) {
        Ok(c) => c,
        Err(_) => return Some(Err(std::io::Error::other("path contains interior NUL"))),
    };
    let args = LinkAtArgs {
        old_dirfd: libc::AT_FDCWD,
        old_path: &old_c,
        new_dirfd: libc::AT_FDCWD,
        new_path: &new_c,
        flags: 0,
    };
    match submit_linkat_blocking(args) {
        Ok(_) => Some(Ok(())),
        Err(e) if e.kind() == std::io::ErrorKind::Unsupported => None,
        Err(e) => Some(Err(e)),
    }
}

#[cfg(not(all(target_os = "linux", feature = "io_uring")))]
fn try_hard_link_via_io_uring_impl(
    _src_path: &std::path::Path,
    _dst_path: &std::path::Path,
) -> Option<std::io::Result<()>> {
    None
}

/// Attempts to stat files via io_uring `IORING_OP_STATX` batch submission.
///
/// On Linux with kernel 5.11+ and io_uring available, submits all paths
/// as independent STATX SQEs on a single ring and returns the results.
/// On all other platforms, or when the kernel lacks the opcode, returns
/// `None` so the caller can fall back to synchronous stat calls.
///
/// # Arguments
///
/// * `paths` - Slice of paths to stat.
/// * `follow_symlinks` - If `true`, follows symlinks (like `stat`);
///   if `false`, does not follow (like `lstat`).
///
/// # Returns
///
/// - `Some(Ok(results))` when io_uring statx is available and all
///   submissions succeeded (individual paths may still have errors).
/// - `None` when io_uring statx is not available on this platform/kernel.
/// - `Some(Err(...))` for ring-level failures.
#[must_use]
pub fn try_statx_batch_via_io_uring(
    paths: &[&std::path::Path],
    follow_symlinks: bool,
) -> Option<std::io::Result<Vec<StatxResult>>> {
    try_statx_batch_via_io_uring_impl(paths, follow_symlinks)
}

#[cfg(all(target_os = "linux", feature = "io_uring"))]
fn try_statx_batch_via_io_uring_impl(
    paths: &[&std::path::Path],
    follow_symlinks: bool,
) -> Option<std::io::Result<Vec<StatxResult>>> {
    if !statx_supported() {
        return None;
    }
    Some(submit_statx_batch(paths, follow_symlinks))
}

#[cfg(not(all(target_os = "linux", feature = "io_uring")))]
fn try_statx_batch_via_io_uring_impl(
    _paths: &[&std::path::Path],
    _follow_symlinks: bool,
) -> Option<std::io::Result<Vec<StatxResult>>> {
    None
}

/// Creates a hard link from `src` to `dst`, trying io_uring first.
///
/// On Linux 5.15+ with io_uring `IORING_OP_LINKAT` support, the link is
/// submitted as an asynchronous SQE on a transient ring, avoiding a
/// synchronous `linkat(2)` syscall. On all other platforms, older kernels,
/// or when the `io_uring` feature is disabled, falls back to
/// [`std::fs::hard_link`].
///
/// This is the recommended single entry point for hard-link creation across
/// the codebase. It consolidates the try-io_uring-then-fallback pattern so
/// callers do not need to handle the `Option` from
/// [`try_hard_link_via_io_uring`] themselves.
///
/// # Errors
///
/// Returns an error when both the io_uring path and the `std::fs::hard_link`
/// fallback fail (e.g., `EEXIST`, `EACCES`, `EXDEV`).
///
/// # Upstream reference
///
/// Upstream rsync uses synchronous `link(2)` / `linkat(2)` for hardlink
/// creation (`hlink.c`). The io_uring fast path is a latency optimisation.
pub fn hard_link(src: &std::path::Path, dst: &std::path::Path) -> std::io::Result<()> {
    if let Some(result) = try_hard_link_via_io_uring(src, dst) {
        return result;
    }
    std::fs::hard_link(src, dst)
}

/// Minimum value accepted for the io_uring submission queue depth tunable.
///
/// The kernel rejects rings with zero entries, so callers must request at
/// least one SQE slot.
pub const IO_URING_DEPTH_MIN: u32 = 1;

/// Maximum value accepted for the io_uring submission queue depth tunable.
///
/// The kernel caps SQ entries at 32768 (2^15) for non-privileged callers, so
/// we surface that as the upper bound for the CLI tunable. Larger values are
/// rejected at parse time rather than at ring construction time.
pub const IO_URING_DEPTH_MAX: u32 = 32768;

/// Errors returned by [`validate_io_uring_depth`] when the requested submission
/// queue depth is outside the supported range or not a power of two.
///
/// Mirrors the kernel's `io_uring_setup(2)` requirements: the depth must be
/// in `[IO_URING_DEPTH_MIN, IO_URING_DEPTH_MAX]` and a power of two.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum IoUringDepthError {
    /// The supplied depth was zero. The kernel requires at least one SQE.
    #[error("--io-uring-depth must be at least {IO_URING_DEPTH_MIN}")]
    Zero,
    /// The supplied depth was not a power of two. `io_uring_setup(2)` rounds
    /// up internally, but rejecting non-powers here keeps behaviour explicit.
    #[error("--io-uring-depth must be a power of two (got {0})")]
    NotPowerOfTwo(u32),
    /// The supplied depth exceeded [`IO_URING_DEPTH_MAX`].
    #[error("--io-uring-depth must be at most {IO_URING_DEPTH_MAX} (got {0})")]
    TooLarge(u32),
}

/// Validates a user-supplied io_uring submission queue depth.
///
/// Accepts powers of two in the inclusive range
/// `[IO_URING_DEPTH_MIN, IO_URING_DEPTH_MAX]`. Returns the validated value on
/// success or an [`IoUringDepthError`] describing why the input is invalid.
///
/// # Examples
///
/// ```
/// use fast_io::{IO_URING_DEPTH_MAX, IoUringDepthError, validate_io_uring_depth};
///
/// assert_eq!(validate_io_uring_depth(256), Ok(256));
/// assert_eq!(validate_io_uring_depth(0), Err(IoUringDepthError::Zero));
/// assert_eq!(
///     validate_io_uring_depth(100),
///     Err(IoUringDepthError::NotPowerOfTwo(100)),
/// );
/// assert_eq!(
///     validate_io_uring_depth(IO_URING_DEPTH_MAX * 2),
///     Err(IoUringDepthError::TooLarge(IO_URING_DEPTH_MAX * 2)),
/// );
/// ```
pub fn validate_io_uring_depth(depth: u32) -> Result<u32, IoUringDepthError> {
    if depth == 0 {
        return Err(IoUringDepthError::Zero);
    }
    if depth > IO_URING_DEPTH_MAX {
        return Err(IoUringDepthError::TooLarge(depth));
    }
    if !depth.is_power_of_two() {
        return Err(IoUringDepthError::NotPowerOfTwo(depth));
    }
    Ok(depth)
}

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

/// Policy controlling copy-on-write reflink usage for whole-file copies.
///
/// This enum allows callers to disable CoW (`FICLONE`/`copy_file_range` on
/// Linux, `clonefile`/`fcopyfile` on macOS, `FSCTL_DUPLICATE_EXTENTS`/
/// `CopyFileExW` on Windows) and force the portable `std::fs::copy`
/// fallback. Useful for benchmarking, diagnostics, or when downstream
/// tooling does not handle reflinks correctly.
///
/// The `--cow` (default) and `--no-cow` CLI flags map onto this enum:
/// - `--cow` selects [`CowPolicy::Auto`].
/// - `--no-cow` selects [`CowPolicy::Disabled`].
///
/// The default is [`CowPolicy::Auto`], which delegates to
/// [`super::DefaultPlatformCopy`] and uses the best available reflink
/// mechanism with portable fallback.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum CowPolicy {
    /// Auto-detect reflink support and use it when available (default).
    ///
    /// Delegates to [`super::DefaultPlatformCopy`] which selects the best
    /// available copy mechanism per platform with portable fallback.
    #[default]
    Auto,
    /// Disable copy-on-write reflinks; always use portable `std::fs::copy`.
    ///
    /// Forces every whole-file copy through the standard buffered fallback,
    /// bypassing `FICLONE`, `copy_file_range`, `clonefile`, `fcopyfile`,
    /// `FSCTL_DUPLICATE_EXTENTS`, and `CopyFileExW`. Useful when destination
    /// filesystems mishandle reflinks or for measuring CoW performance gains.
    Disabled,
}

/// Policy controlling IOCP usage for file I/O on Windows.
///
/// This enum allows callers to explicitly enable, disable, or auto-detect
/// IOCP support. It mirrors [`IoUringPolicy`] for the Windows platform.
///
/// # Runtime detection
///
/// When set to `Auto`, the runtime check ([`iocp::is_iocp_available`])
/// creates a test completion port and caches the result. On Windows Vista+,
/// IOCP is always available. Files smaller than 64 KB use standard I/O
/// regardless of this policy since the async overhead exceeds the benefit.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum IocpPolicy {
    /// Auto-detect IOCP availability at runtime (default).
    ///
    /// Uses IOCP on Windows when the `iocp` feature is enabled.
    /// Falls back to standard buffered I/O otherwise.
    #[default]
    Auto,
    /// Force IOCP usage. Returns an error if IOCP is unavailable.
    ///
    /// Useful for testing or when IOCP is required for performance.
    /// Fails with `ErrorKind::Unsupported` on non-Windows platforms.
    Enabled,
    /// Disable IOCP; always use standard buffered I/O.
    ///
    /// Useful for benchmarking or diagnosing IOCP-related issues.
    Disabled,
}

/// Policy controlling I/O-level zero-copy syscalls (`sendfile`, `splice`,
/// `copy_file_range`, io_uring `IORING_OP_SEND_ZC`).
///
/// This enum gates kernel zero-copy data movement between file descriptors
/// and sockets. It is orthogonal to filesystem-level reflink/CoW cloning
/// (controlled by the separate cow policy). When [`ZeroCopyPolicy::Disabled`]
/// is in effect, callers route through standard userspace `read`/`write`
/// loops; the wrapped [`DefaultPlatformCopy`] strategy is replaced by
/// [`NoZeroCopyPlatformCopy`] which forces a portable buffered copy.
///
/// # Precedence with the cow policy
///
/// `--cow` controls FS-level extent sharing (`FICLONE`, `clonefile`, ReFS
/// `FSCTL_DUPLICATE_EXTENTS_TO_FILE`). `--zero-copy` controls IO-level
/// kernel-side data movement (`sendfile`, `splice`, `copy_file_range`,
/// `SEND_ZC`). The two are independent: a transfer can use reflink without
/// `sendfile` (whole-file CoW clone) or `sendfile` without reflink (network
/// send of a file). Disabling either does not affect the other.
///
/// # Runtime fallback chain
///
/// When set to [`ZeroCopyPolicy::Auto`], the platform fallback chain in
/// [`platform_copy::DefaultPlatformCopy`] selects the best mechanism. When
/// set to [`ZeroCopyPolicy::Disabled`], the chain is bypassed and callers
/// use [`std::fs::copy`] (which still uses kernel optimizations on some
/// platforms but skips `copy_file_range`/`sendfile`/`splice` direct calls).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ZeroCopyPolicy {
    /// Auto-detect zero-copy availability at runtime (default).
    ///
    /// Uses `sendfile`, `splice`, `copy_file_range`, and io_uring `SEND_ZC`
    /// when the kernel supports them. Falls back to standard buffered I/O
    /// otherwise. This is the recommended setting for production use.
    #[default]
    Auto,
    /// Force zero-copy usage where supported.
    ///
    /// Useful for testing or benchmarking the zero-copy code paths.
    /// On platforms or filesystems that do not support a particular
    /// syscall, the call still falls back to standard I/O - this policy
    /// does not error, it simply opts in to the optimization where
    /// possible.
    Enabled,
    /// Disable zero-copy; always use standard buffered read/write.
    ///
    /// Useful for benchmarking, diagnosing zero-copy related issues,
    /// or working around kernels where `sendfile`/`splice` are blocked
    /// by seccomp filters. When set, callers route through portable
    /// userspace copy loops and io_uring socket sends fall back from
    /// `IORING_OP_SEND_ZC` to `IORING_OP_SEND`.
    Disabled,
}

#[cfg(test)]
mod io_uring_depth_tests {
    use super::*;

    #[test]
    fn validate_io_uring_depth_accepts_default() {
        assert_eq!(validate_io_uring_depth(64), Ok(64));
    }

    #[test]
    fn validate_io_uring_depth_accepts_power_of_two() {
        for &depth in &[1u32, 2, 4, 8, 16, 32, 256, 1024, 4096, IO_URING_DEPTH_MAX] {
            assert_eq!(validate_io_uring_depth(depth), Ok(depth));
        }
    }

    #[test]
    fn validate_io_uring_depth_rejects_zero() {
        assert_eq!(validate_io_uring_depth(0), Err(IoUringDepthError::Zero));
    }

    #[test]
    fn validate_io_uring_depth_rejects_non_power_of_two() {
        assert_eq!(
            validate_io_uring_depth(100),
            Err(IoUringDepthError::NotPowerOfTwo(100)),
        );
        assert_eq!(
            validate_io_uring_depth(3),
            Err(IoUringDepthError::NotPowerOfTwo(3)),
        );
    }

    #[test]
    fn validate_io_uring_depth_rejects_too_large() {
        let too_large = IO_URING_DEPTH_MAX * 2;
        assert_eq!(
            validate_io_uring_depth(too_large),
            Err(IoUringDepthError::TooLarge(too_large)),
        );
    }

    #[test]
    fn io_uring_depth_error_messages_mention_flag() {
        assert!(
            IoUringDepthError::Zero
                .to_string()
                .contains("--io-uring-depth")
        );
        assert!(
            IoUringDepthError::NotPowerOfTwo(7)
                .to_string()
                .contains("--io-uring-depth")
        );
        assert!(
            IoUringDepthError::TooLarge(IO_URING_DEPTH_MAX + 1)
                .to_string()
                .contains("--io-uring-depth")
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn platform_io_capabilities_returns_expected_entries() {
        let caps = platform_io_capabilities();

        #[cfg(target_os = "linux")]
        {
            assert!(caps.contains(&"copy_file_range"));
            assert!(caps.contains(&"sendfile"));
            assert!(caps.contains(&"FICLONE"));
            assert!(caps.contains(&"O_TMPFILE"));
        }

        #[cfg(target_os = "macos")]
        {
            assert!(caps.contains(&"clonefile"));
            assert!(caps.contains(&"fcopyfile"));
            assert!(caps.contains(&"F_NOCACHE"));
            assert!(caps.contains(&"writev"));
        }

        #[cfg(target_os = "windows")]
        {
            assert!(caps.contains(&"CopyFileEx"));
            assert!(caps.contains(&"ReFS reflink"));
        }
    }

    #[test]
    fn iocp_status_detail_returns_non_empty_string() {
        let detail = iocp_status_detail();
        assert!(!detail.is_empty());

        #[cfg(not(target_os = "windows"))]
        assert!(detail.contains("not available"));

        #[cfg(all(target_os = "windows", not(feature = "iocp")))]
        assert!(detail.contains("not compiled in"));

        #[cfg(all(target_os = "windows", feature = "iocp"))]
        assert!(detail.contains("compiled in"));
    }

    #[test]
    fn iocp_status_detail_is_single_line() {
        let detail = iocp_status_detail();
        assert!(!detail.contains('\n'));
    }

    #[test]
    fn iocp_status_detail_no_trailing_whitespace() {
        let detail = iocp_status_detail();
        assert_eq!(detail, detail.trim());
    }

    #[test]
    fn io_uring_status_detail_returns_non_empty_string() {
        let detail = io_uring_status_detail();
        assert!(!detail.is_empty());

        #[cfg(not(target_os = "linux"))]
        assert!(detail.contains("not available"));

        #[cfg(all(target_os = "linux", not(feature = "io_uring")))]
        assert!(detail.contains("not compiled in"));

        #[cfg(all(target_os = "linux", feature = "io_uring"))]
        assert!(detail.contains("compiled in"));
    }

    #[test]
    fn io_uring_availability_reason_returns_non_empty_string() {
        let reason = io_uring_availability_reason();
        assert!(!reason.is_empty());
        assert!(reason.starts_with("io_uring: "));

        #[cfg(not(target_os = "linux"))]
        assert!(reason.contains("not Linux"));

        #[cfg(all(target_os = "linux", not(feature = "io_uring")))]
        assert!(reason.contains("not compiled in"));

        #[cfg(all(target_os = "linux", feature = "io_uring"))]
        {
            // Must contain either "enabled" or "disabled"
            assert!(reason.contains("enabled") || reason.contains("disabled"));
        }
    }

    #[test]
    fn platform_io_capabilities_has_no_duplicates() {
        let caps = platform_io_capabilities();
        let mut seen = std::collections::HashSet::new();
        for cap in &caps {
            assert!(seen.insert(cap), "duplicate capability: {cap}");
        }
    }

    #[test]
    fn zero_copy_policy_default_is_auto() {
        assert_eq!(ZeroCopyPolicy::default(), ZeroCopyPolicy::Auto);
    }

    #[test]
    fn zero_copy_policy_variants_are_distinct() {
        assert_ne!(ZeroCopyPolicy::Auto, ZeroCopyPolicy::Enabled);
        assert_ne!(ZeroCopyPolicy::Auto, ZeroCopyPolicy::Disabled);
        assert_ne!(ZeroCopyPolicy::Enabled, ZeroCopyPolicy::Disabled);
    }

    #[test]
    fn is_splice_enabled_respects_disabled_policy() {
        assert!(!is_splice_enabled(ZeroCopyPolicy::Disabled));
    }

    #[test]
    fn is_splice_enabled_auto_matches_availability() {
        assert_eq!(
            is_splice_enabled(ZeroCopyPolicy::Auto),
            is_splice_available()
        );
    }
}

#[cfg(test)]
mod io_uring_fallback_tests {
    use super::*;

    #[cfg(not(target_os = "linux"))]
    #[test]
    fn io_uring_unavailable_on_non_linux() {
        assert!(
            !is_io_uring_available(),
            "io_uring must not be available on non-Linux platforms"
        );
    }

    #[cfg(not(target_os = "linux"))]
    #[test]
    fn io_uring_status_detail_indicates_platform_unavailability() {
        let detail = io_uring_status_detail();
        assert!(
            detail.contains("not available"),
            "status detail must indicate unavailability on non-Linux, got: {detail}"
        );
        assert!(
            detail.contains("not Linux"),
            "status detail must mention platform is not Linux, got: {detail}"
        );
    }

    #[cfg(not(target_os = "linux"))]
    #[test]
    fn io_uring_availability_reason_describes_platform_constraint() {
        let reason = io_uring_availability_reason();
        assert!(
            reason.starts_with("io_uring: disabled"),
            "reason must start with 'io_uring: disabled' on non-Linux, got: {reason}"
        );
        assert!(
            reason.contains("not Linux"),
            "reason must explain platform is not Linux, got: {reason}"
        );
    }

    #[cfg(not(target_os = "linux"))]
    #[test]
    fn io_uring_fallback_produces_no_errors() {
        // Verify that querying io_uring status on non-Linux does not panic or error -
        // the fallback path is exercised cleanly.
        let available = is_io_uring_available();
        let detail = io_uring_status_detail();
        let reason = io_uring_availability_reason();

        assert!(!available);
        assert!(!detail.is_empty());
        assert!(!reason.is_empty());
    }

    #[cfg(not(target_os = "linux"))]
    #[test]
    fn io_uring_capabilities_excluded_on_non_linux() {
        let caps = platform_io_capabilities();
        assert!(
            !caps.contains(&"io_uring"),
            "io_uring must not appear in capabilities on non-Linux"
        );
    }

    #[cfg(all(target_os = "linux", feature = "io_uring"))]
    #[test]
    fn io_uring_status_detail_well_formed_on_linux() {
        let detail = io_uring_status_detail();
        assert!(
            detail.starts_with("compiled in, "),
            "Linux+feature status must start with 'compiled in, ', got: {detail}"
        );
        assert!(
            detail.contains("available") || detail.contains("unavailable"),
            "status detail must indicate availability state, got: {detail}"
        );
    }

    #[cfg(all(target_os = "linux", feature = "io_uring"))]
    #[test]
    fn io_uring_availability_reason_well_formed_on_linux() {
        let reason = io_uring_availability_reason();
        assert!(
            reason.starts_with("io_uring: "),
            "reason must start with 'io_uring: ', got: {reason}"
        );
        // On Linux with the feature, the reason must mention the kernel version
        // or a specific unavailability cause.
        let has_kernel_info = reason.contains("kernel");
        let has_parse_error = reason.contains("could not");
        assert!(
            has_kernel_info || has_parse_error,
            "reason must contain kernel info or parse error, got: {reason}"
        );
    }

    #[cfg(all(target_os = "linux", feature = "io_uring"))]
    #[test]
    fn io_uring_availability_consistent_with_reason() {
        let available = is_io_uring_available();
        let reason = io_uring_availability_reason();

        if available {
            assert!(
                reason.contains("enabled"),
                "reason must say 'enabled' when io_uring is available, got: {reason}"
            );
            assert!(
                !reason.contains("disabled"),
                "reason must not say 'disabled' when io_uring is available, got: {reason}"
            );
        } else {
            assert!(
                reason.contains("disabled"),
                "reason must say 'disabled' when io_uring is not available, got: {reason}"
            );
        }
    }

    #[cfg(all(target_os = "linux", not(feature = "io_uring")))]
    #[test]
    fn io_uring_feature_disabled_status() {
        let detail = io_uring_status_detail();
        assert!(
            detail.contains("not compiled in"),
            "status must indicate feature not compiled when io_uring feature disabled, got: {detail}"
        );

        let reason = io_uring_availability_reason();
        assert!(
            reason.contains("not compiled in"),
            "reason must indicate feature not compiled, got: {reason}"
        );
    }

    #[test]
    fn io_uring_status_detail_is_single_line() {
        let detail = io_uring_status_detail();
        assert!(
            !detail.contains('\n'),
            "status detail must be a single line for display purposes, got: {detail}"
        );
    }

    #[test]
    fn io_uring_availability_reason_is_single_line() {
        let reason = io_uring_availability_reason();
        assert!(
            !reason.contains('\n'),
            "availability reason must be a single line for log output, got: {reason}"
        );
    }

    #[test]
    fn io_uring_availability_reason_starts_with_io_uring_prefix() {
        let reason = io_uring_availability_reason();
        assert!(
            reason.starts_with("io_uring: "),
            "reason must start with 'io_uring: ' prefix for consistent log formatting, got: {reason}"
        );
    }

    #[test]
    fn io_uring_status_detail_no_trailing_whitespace() {
        let detail = io_uring_status_detail();
        assert_eq!(
            detail,
            detail.trim(),
            "status detail must not have leading/trailing whitespace"
        );
    }

    #[test]
    fn io_uring_availability_reason_no_trailing_whitespace() {
        let reason = io_uring_availability_reason();
        assert_eq!(
            reason,
            reason.trim(),
            "availability reason must not have leading/trailing whitespace"
        );
    }

    #[test]
    fn sqpoll_fell_back_starts_as_false() {
        // SQPOLL fallback flag must default to false - it is only set when
        // SQPOLL setup is attempted and fails on a Linux kernel.
        assert!(
            !sqpoll_fell_back(),
            "sqpoll_fell_back() must be false at startup"
        );
    }

    #[cfg(not(target_os = "linux"))]
    #[test]
    fn factory_reader_falls_back_to_std_on_non_linux() {
        use crate::traits::FileReaderFactory;
        use io_uring::{IoUringOrStdReader, IoUringReaderFactory};

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("factory_fallback_reader.txt");
        std::fs::write(&path, b"fallback test content").unwrap();

        let factory = IoUringReaderFactory::default();
        assert!(
            !factory.will_use_io_uring(),
            "factory must not use io_uring on non-Linux"
        );

        let reader = factory.open(&path).unwrap();
        assert!(
            matches!(reader, IoUringOrStdReader::Std(_)),
            "reader must be Std variant on non-Linux"
        );
    }

    #[cfg(not(target_os = "linux"))]
    #[test]
    fn factory_writer_falls_back_to_std_on_non_linux() {
        use crate::traits::FileWriterFactory;
        use io_uring::{IoUringOrStdWriter, IoUringWriterFactory};

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("factory_fallback_writer.txt");

        let factory = IoUringWriterFactory::default();
        assert!(
            !factory.will_use_io_uring(),
            "factory must not use io_uring on non-Linux"
        );

        let writer = factory.create(&path).unwrap();
        assert!(
            matches!(writer, IoUringOrStdWriter::Std(_)),
            "writer must be Std variant on non-Linux"
        );
    }

    #[cfg(not(target_os = "linux"))]
    #[test]
    fn factory_writer_create_with_size_falls_back_to_std_on_non_linux() {
        use crate::traits::FileWriterFactory;
        use io_uring::{IoUringOrStdWriter, IoUringWriterFactory};

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("factory_fallback_sized.txt");

        let factory = IoUringWriterFactory::default();
        let writer = factory.create_with_size(&path, 4096).unwrap();
        assert!(
            matches!(writer, IoUringOrStdWriter::Std(_)),
            "sized writer must be Std variant on non-Linux"
        );
    }
}

#[cfg(test)]
mod io_uring_rename_dispatch_tests {
    use super::*;
    use std::fs;

    #[test]
    fn try_rename_via_io_uring_renames_or_returns_none() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("rename_src.txt");
        let dst = dir.path().join("rename_dst.txt");
        fs::write(&src, b"rename payload").unwrap();

        match try_rename_via_io_uring(&src, &dst) {
            Some(Ok(())) => {
                // io_uring path succeeded - verify file moved.
                assert!(!src.exists());
                assert_eq!(fs::read(&dst).unwrap(), b"rename payload");
            }
            Some(Err(e)) => {
                panic!("io_uring rename returned error: {e}");
            }
            None => {
                // Not available on this platform/kernel - file untouched.
                assert!(src.exists());
                assert!(!dst.exists());
            }
        }
    }

    #[test]
    fn try_rename_via_io_uring_returns_none_consistently() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("consistency_src.txt");
        let dst = dir.path().join("consistency_dst.txt");
        fs::write(&src, b"data").unwrap();

        let first = try_rename_via_io_uring(&src, &dst).is_some();
        // If first call consumed the file, recreate for second probe.
        if first {
            fs::write(&src, b"data").unwrap();
            let _ = fs::remove_file(&dst);
        }
        let second = try_rename_via_io_uring(&src, &dst).is_some();
        assert_eq!(
            first, second,
            "availability must be consistent across calls"
        );
    }

    #[cfg(not(target_os = "linux"))]
    #[test]
    fn try_rename_via_io_uring_returns_none_on_non_linux() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("non_linux_src.txt");
        let dst = dir.path().join("non_linux_dst.txt");
        fs::write(&src, b"data").unwrap();

        assert!(
            try_rename_via_io_uring(&src, &dst).is_none(),
            "must return None on non-Linux platforms"
        );
        assert!(src.exists(), "source must be untouched");
    }
}

#[cfg(test)]
mod io_uring_hard_link_dispatch_tests {
    use super::*;
    use std::fs;

    #[test]
    fn try_hard_link_via_io_uring_links_or_returns_none() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("link_src.txt");
        let dst = dir.path().join("link_dst.txt");
        fs::write(&src, b"link payload").unwrap();

        match try_hard_link_via_io_uring(&src, &dst) {
            Some(Ok(())) => {
                // io_uring path succeeded - verify hard link created.
                assert!(src.exists());
                assert!(dst.exists());
                assert_eq!(fs::read(&dst).unwrap(), b"link payload");
            }
            Some(Err(e)) => {
                panic!("io_uring hard_link returned error: {e}");
            }
            None => {
                // Not available on this platform/kernel.
                assert!(src.exists());
                assert!(!dst.exists());
            }
        }
    }

    #[test]
    fn try_hard_link_via_io_uring_returns_none_consistently() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("link_consistency_src.txt");
        let dst1 = dir.path().join("link_consistency_dst1.txt");
        let dst2 = dir.path().join("link_consistency_dst2.txt");
        fs::write(&src, b"data").unwrap();

        let first = try_hard_link_via_io_uring(&src, &dst1).is_some();
        let second = try_hard_link_via_io_uring(&src, &dst2).is_some();
        assert_eq!(
            first, second,
            "availability must be consistent across calls"
        );
    }

    #[cfg(not(target_os = "linux"))]
    #[test]
    fn try_hard_link_via_io_uring_returns_none_on_non_linux() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("non_linux_link_src.txt");
        let dst = dir.path().join("non_linux_link_dst.txt");
        fs::write(&src, b"data").unwrap();

        assert!(
            try_hard_link_via_io_uring(&src, &dst).is_none(),
            "must return None on non-Linux platforms"
        );
        assert!(!dst.exists(), "destination must not exist");
    }
}

#[cfg(test)]
mod io_uring_statx_dispatch_tests {
    use super::*;
    use std::fs;

    #[test]
    fn try_statx_batch_via_io_uring_stats_or_returns_none() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("statx_dispatch.txt");
        fs::write(&file, b"dispatch payload").unwrap();

        let paths: Vec<&std::path::Path> = vec![file.as_path()];
        match try_statx_batch_via_io_uring(&paths, true) {
            Some(Ok(results)) => {
                assert_eq!(results.len(), 1);
                assert!(results[0].is_ok(), "existing file should succeed");
            }
            Some(Err(e)) => {
                panic!("io_uring statx batch returned ring error: {e}");
            }
            None => {
                // Not available on this platform/kernel.
            }
        }
    }

    #[test]
    fn try_statx_batch_via_io_uring_returns_none_consistently() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("statx_consistency.txt");
        fs::write(&file, b"data").unwrap();

        let paths: Vec<&std::path::Path> = vec![file.as_path()];
        let first = try_statx_batch_via_io_uring(&paths, true).is_some();
        let second = try_statx_batch_via_io_uring(&paths, true).is_some();
        assert_eq!(
            first, second,
            "availability must be consistent across calls"
        );
    }

    #[cfg(not(target_os = "linux"))]
    #[test]
    fn try_statx_batch_via_io_uring_returns_none_on_non_linux() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("non_linux_statx.txt");
        fs::write(&file, b"data").unwrap();

        let paths: Vec<&std::path::Path> = vec![file.as_path()];
        assert!(
            try_statx_batch_via_io_uring(&paths, true).is_none(),
            "must return None on non-Linux platforms"
        );
    }

    #[test]
    fn try_statx_batch_via_io_uring_empty_input() {
        let paths: Vec<&std::path::Path> = vec![];
        match try_statx_batch_via_io_uring(&paths, true) {
            Some(Ok(results)) => {
                assert!(results.is_empty());
            }
            None => {
                // Not available on this platform.
            }
            Some(Err(e)) => {
                panic!("unexpected error on empty input: {e}");
            }
        }
    }
}

#[cfg(test)]
mod hard_link_convenience_tests {
    use super::*;
    use std::fs;

    /// Verifies `hard_link` creates a valid hard link on any platform,
    /// using io_uring when available and falling back to `std::fs::hard_link`.
    #[test]
    fn hard_link_creates_link() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("hl_src.txt");
        let dst = dir.path().join("hl_dst.txt");
        fs::write(&src, b"hard link payload").unwrap();

        hard_link(&src, &dst).unwrap();

        assert!(src.exists(), "source must still exist after hard link");
        assert!(dst.exists(), "destination must exist after hard link");
        assert_eq!(fs::read(&dst).unwrap(), b"hard link payload");
    }

    /// Verifies that source and destination share the same inode on Unix,
    /// confirming a true hard link rather than a copy.
    #[cfg(unix)]
    #[test]
    fn hard_link_shares_inode() {
        use std::os::unix::fs::MetadataExt;

        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("hl_inode_src.txt");
        let dst = dir.path().join("hl_inode_dst.txt");
        fs::write(&src, b"inode check").unwrap();

        hard_link(&src, &dst).unwrap();

        let src_ino = fs::metadata(&src).unwrap().ino();
        let dst_ino = fs::metadata(&dst).unwrap().ino();
        assert_eq!(src_ino, dst_ino, "hard link must share same inode");
    }

    /// Verifies `hard_link` returns an error when the destination already
    /// exists (EEXIST).
    #[test]
    fn hard_link_fails_when_dst_exists() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("hl_exists_src.txt");
        let dst = dir.path().join("hl_exists_dst.txt");
        fs::write(&src, b"source").unwrap();
        fs::write(&dst, b"existing").unwrap();

        let result = hard_link(&src, &dst);
        assert!(result.is_err(), "must fail when destination exists");
    }

    /// Verifies `hard_link` returns an error when the source does not exist.
    #[test]
    fn hard_link_fails_for_missing_source() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("hl_missing.txt");
        let dst = dir.path().join("hl_missing_dst.txt");

        let result = hard_link(&src, &dst);
        assert!(result.is_err(), "must fail when source does not exist");
    }

    /// Verifies that writing to the source after hard-linking is visible
    /// through the destination path, confirming shared data blocks.
    #[test]
    fn hard_link_shares_data() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("hl_shared_src.txt");
        let dst = dir.path().join("hl_shared_dst.txt");
        fs::write(&src, b"original").unwrap();

        hard_link(&src, &dst).unwrap();

        // Overwrite via source path.
        fs::write(&src, b"modified").unwrap();

        // Read through destination - should see the modification.
        assert_eq!(fs::read(&dst).unwrap(), b"modified");
    }
}
