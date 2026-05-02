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
    CopyMethod, CopyResult, DefaultPlatformCopy, PlatformCopy, try_clonefile, try_fcopyfile,
    try_ficlone, try_refs_reflink,
};
pub use traits::{FileReader, FileWriter};

pub use kernel_version::{
    IO_URING_MIN_KERNEL, KernelVersion, log_io_uring_probe_result, parse_kernel_version,
};
pub use refs_detect::{clear_refs_cache, is_refs_filesystem};
pub use socket_options::set_socket_int_option;
pub use splice::{is_splice_available, recv_fd_to_file, try_splice_to_file};

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
    BufferRing, BufferRingConfig, BufferRingError, IoUringConfig, IoUringDiskBatch,
    IoUringKernelInfo, IoUringOrStdReader, IoUringOrStdWriter, IoUringReader, IoUringReaderFactory,
    IoUringWriter, IoUringWriterFactory, OpTag, RegisteredBufferGroup, RegisteredBufferSlot,
    RegisteredBufferStats, SharedCompletion, SharedRing, SharedRingConfig,
    buffer_id_from_cqe_flags, is_io_uring_available, reader_from_path, sqpoll_fell_back,
    writer_from_file,
};

#[cfg(all(target_os = "windows", feature = "iocp"))]
pub use iocp::post_completion as iocp_post_completion;
pub use iocp::{
    CompletionHandler, CompletionPump, IocpConfig, IocpOrStdReader, IocpOrStdWriter,
    IocpPumpConfig, IocpReader, IocpReaderFactory, IocpWriter, IocpWriterFactory,
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
/// - **macOS**: `clonefile`, `fcopyfile`
/// - **Windows**: `CopyFileEx`, `IOCP` (runtime-probed)
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
    }

    // Windows compile-time capabilities
    #[cfg(target_os = "windows")]
    {
        caps.push("CopyFileEx");
        if is_iocp_available() {
            caps.push("IOCP");
        }
    }

    caps
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
        }

        #[cfg(target_os = "windows")]
        {
            assert!(caps.contains(&"CopyFileEx"));
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
