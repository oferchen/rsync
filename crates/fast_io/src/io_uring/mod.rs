//! io_uring-based async file and socket I/O for Linux 5.6+.
//!
//! This module provides high-performance I/O using Linux's io_uring interface,
//! which batches syscalls and enables true async I/O without thread pools.
//!
//! # Kernel requirements
//!
//! - **Linux 5.6 or later** - Minimum for `io_uring_setup(2)` and all opcodes
//!   used by this module (`IORING_OP_READ`, `IORING_OP_WRITE`, `IORING_OP_SEND`).
//! - **`io_uring` cargo feature** must be enabled at compile time.
//! - **No seccomp blocking** - Some container runtimes (Docker with default
//!   seccomp profile before v20.10.2, gVisor) block io_uring syscalls.
//!   The runtime probe in [`is_io_uring_available`] detects this.
//!
//! # Runtime detection and fallback
//!
//! Availability is checked once per process via [`is_io_uring_available`] and
//! cached in a process-wide atomic. The check:
//!
//! 1. Parses `uname().release` for major.minor >= 5.6
//! 2. Attempts `IoUring::new(4)` to verify the syscall is not blocked
//!
//! Factory types ([`IoUringReaderFactory`], [`IoUringWriterFactory`]) and the
//! top-level helpers ([`reader_from_path`], [`writer_from_file`]) automatically
//! fall back to standard buffered I/O (`BufReader`/`BufWriter`) when io_uring
//! is unavailable or ring creation fails.
//!
//! On non-Linux platforms or when the `io_uring` feature is disabled, the stub
//! module (`io_uring_stub.rs`) provides the same public API with
//! `is_io_uring_available()` always returning `false`.
//!
//! # Batching strategy
//!
//! The core advantage of io_uring is amortizing syscall overhead by submitting
//! multiple I/O operations in a single `submit_and_wait()` call. This module
//! implements two batched methods:
//!
//! - [`IoUringReader::read_all_batched`]: Submits up to `sq_entries` concurrent
//!   reads at different file offsets, processes all completions, then repeats
//!   until the entire file is read. A single large file read may need only
//!   `ceil(file_size / (buffer_size * sq_entries))` syscalls instead of
//!   `ceil(file_size / buffer_size)`.
//!
//! - [`IoUringWriter::write_all_batched`]: Splits a contiguous buffer into
//!   chunk-sized SQEs, submits them all at once, and processes completions.
//!   The `flush()` implementation uses this for the internal write buffer.
//!
//! Single-operation methods (`read_at`, `write_at`) are retained as convenience
//! wrappers for callers that need one-off positioned I/O.
//!
//! # Optional features
//!
//! - **`IORING_REGISTER_FILES`** (fd registration) - Enabled by default via
//!   [`IoUringConfig::register_files`]. Eliminates per-SQE kernel file table
//!   lookups, saving ~50ns per operation on high-fd-count processes.
//! - **`IORING_SETUP_SQPOLL`** - Opt-in via [`IoUringConfig::sqpoll`]. A kernel
//!   thread polls the submission queue, eliminating `io_uring_enter` syscalls.
//!   Requires `CAP_SYS_NICE` or root; falls back to normal submission on
//!   `EPERM`.
//!
//! # Privilege requirements
//!
//! | Feature | Privilege | Notes |
//! |---------|-----------|-------|
//! | Base io_uring | None (Linux 5.6+) | Blocked by seccomp in some container runtimes (Docker < 20.10.2, gVisor) |
//! | SQPOLL (`IORING_SETUP_SQPOLL`) | `CAP_SYS_NICE` or root | Falls back transparently to regular submission on `EPERM` |
//! | Registered buffers (`IORING_REGISTER_BUFFERS`) | None | Pins pages in kernel; falls back to regular `Read`/`Write` opcodes on failure |
//! | File registration (`IORING_REGISTER_FILES`) | None | Eliminates per-SQE file table lookups |
//! | Direct I/O (`O_DIRECT`) | None | Requires filesystem support (not tmpfs); alignment constraints apply |
//! | Container / seccomp | N/A | `io_uring_setup(2)` may be blocked entirely; detected once at startup by [`is_io_uring_available`] |
//!
//! # Fallback chain
//!
//! Each layer degrades independently so that io_uring features are best-effort:
//!
//! - **Ring creation**: SQPOLL ring -> regular io_uring ring -> standard buffered I/O.
//!   Factory types handle the final fallback to `BufReader`/`BufWriter`.
//! - **Buffer registration**: registered (`READ_FIXED`/`WRITE_FIXED`) -> regular
//!   (`Read`/`Write`) opcodes. Silent fallback on registration failure.
//! - **Provided-buffer rings (PBUF_RING)**: Linux 5.19+ ring-mapped supplied
//!   buffers (`IORING_REGISTER_PBUF_RING`, opcode 22) -> classic provide-buffers
//!   path on 5.6+ -> standard `read`/`write` -> non-Linux io_uring stub. The
//!   probe is cached process-wide via [`buffer_ring::pbuf_ring_supported`] and
//!   surfaced on [`IoUringKernelInfo::pbuf_ring_supported`]. See
//!   `docs/audits/iouring-pbuf-ring.md` for the full call-site survey.

mod batching;
/// Per-thread BGID lease over the BGE-4 central pool (IUR-3.e).
///
/// Amortises [`BgidAllocator`] mutex acquisitions across per-thread
/// io_uring consumers: each thread leases a slice of bgids on first use
/// and returns them on thread teardown.
pub mod bgid_lease;
/// io_uring provided buffer ring (PBUF_RING) for zero-copy reads.
pub mod buffer_ring;
/// io_uring `IORING_OP_ASYNC_CANCEL` primitive for in-flight SQE cancellation.
pub mod cancel;
mod config;
#[cfg(feature = "iouring-data-reads")]
mod data_reader;
mod disk_batch;
mod file_factory;
mod file_reader;
mod file_writer;
/// io_uring `LINKAT` opcode wrapper and kernel availability probe.
pub mod linkat;
/// Linked SQE chains for the read -> checksum -> write pipeline.
pub mod linked_chain;
/// Per-thread io_uring ring primitive (IUR-3.a).
///
/// Lazy-init thread-local ring used by the migration of `file_writer`,
/// `file_reader`, and `socket_writer` factories to the hybrid per-thread
/// topology chosen by IUR-2. See `docs/design/iur-2-per-thread-rings.md`.
pub mod per_thread_ring;
/// Page-aligned buffer registration for io_uring `READ_FIXED`/`WRITE_FIXED`.
pub mod registered_buffers;
/// `IORING_OP_RENAMEAT` (RENAMEAT2) submission helpers and kernel probe.
pub mod renameat2;
/// `IORING_OP_SEND_ZC` zero-copy socket-send primitive (Linux 6.0+).
pub mod send_zc;
/// Pool of long-lived io_uring instances shared across consumers in a session.
pub mod session_pool;
/// Single io_uring ring shared by a reader fd and a writer fd in one session.
pub mod shared_ring;
mod socket_factory;
mod socket_reader;
mod socket_writer;
/// io_uring `IORING_OP_STATX` opcode wrapper and batch submission.
pub mod statx;

#[cfg(test)]
mod tests;

use std::fs::File;
use std::io::{self, Write};

pub use bgid_lease::{BgidLease, DEFAULT_LEASE_BATCH, with_thread_lease};
pub use buffer_ring::{
    BgidAllocError, BgidAllocator, BgidSessionStats, BgidSnapshot, BufferRing, BufferRingConfig,
    BufferRingError, bgid_exhausted_count, bgid_inflight, bgid_peak_used, bgid_snapshot,
    buffer_id_from_cqe_flags, pbuf_ring_supported,
};
pub use cancel::{
    ASYNC_CANCEL_FD_MIN_KERNEL, ASYNC_CANCEL_MIN_KERNEL, CancelOutcome, IORING_OP_ASYNC_CANCEL,
    cancel_all_by_fd, cancel_by_user_data,
};
pub use config::{
    IoUringConfig, IoUringKernelInfo, config_detail, is_io_uring_available, sqpoll_fell_back,
};
#[cfg(feature = "iouring-data-reads")]
pub use data_reader::IoUringFileReader;
pub use disk_batch::IoUringDiskBatch;
pub use file_factory::{
    IoUringOrStdReader, IoUringOrStdWriter, IoUringReaderFactory, IoUringWriterFactory,
};
pub use file_reader::IoUringReader;
pub use file_writer::IoUringWriter;
pub use linkat::{
    IORING_OP_LINKAT, LINKAT_MIN_KERNEL, LinkAtArgs, build_linkat_sqe, build_linkat_sqe_unchecked,
    linkat_supported, submit_linkat_blocking,
};
pub use linked_chain::{CqeResult, LinkedChain, read_then_write};
pub use per_thread_ring::{DEFAULT_RING_DEPTH as PER_THREAD_RING_DEPTH, PerThreadRing};
pub use registered_buffers::{
    RegisteredBufferGroup, RegisteredBufferSlot, RegisteredBufferStats, RegisteredBufferStatus,
};
#[cfg(feature = "iouring-send-zc")]
pub use send_zc::{SEND_ZC_DISPATCH_MIN_BYTES, ZeroCopySender};
pub use send_zc::{is_supported as send_zc_supported, try_send_zc};

/// Internal re-exports of the underlying `io-uring` crate types used by
/// `#[doc(hidden)]` test helpers (see `registered_buffers::submit_read_fixed_batch`).
/// These are exposed so integration tests in `crates/fast_io/tests/` can drive
/// the fixed-buffer batch path without taking a separate dev-dependency on the
/// `io-uring` crate. Not part of the stable public API.
#[doc(hidden)]
pub mod __test_reexports {
    pub use ::io_uring::IoUring as RawIoUring;
    pub use ::io_uring::types::Fd;
}

#[doc(hidden)]
pub use registered_buffers::{RegisteredBufferSlotInfo, submit_read_fixed_batch};
pub use renameat2::{
    IORING_OP_RENAMEAT, RENAME_EXCHANGE, RENAME_NOREPLACE, RENAME_WHITEOUT, RenameAt2Args,
    build_renameat2_sqe, build_renameat2_sqe_unchecked, renameat2_blocking, renameat2_supported,
};
pub use session_pool::{
    RingLease, SessionPoolConfig, SessionRingPool, ThreadLocalRingLease, ThreadLocalRingPool,
};
pub use shared_ring::{OpTag, SharedCompletion, SharedRing, SharedRingConfig};
pub use socket_factory::{
    IoUringOrStdSocketReader, IoUringOrStdSocketWriter, socket_reader_from_fd,
    socket_writer_from_fd,
};
pub use socket_reader::IoUringSocketReader;
pub use socket_writer::IoUringSocketWriter;
pub use statx::{
    IORING_OP_STATX, STATX_MIN_KERNEL, StatxArgs, StatxResult, build_statx_sqe,
    build_statx_sqe_unchecked, statx_supported, submit_statx_batch, submit_statx_blocking,
};

use crate::io_uring_common::IoBackend;
use crate::traits::{FileReader, FileReaderFactory, FileWriterFactory};

/// Marker type implementing [`IoBackend`] for the live Linux io_uring backend.
///
/// Provides the cross-platform `IoBackend` view of this module so callers can
/// query availability through a single trait regardless of whether the
/// process was compiled with the Linux backend or the no-op stub.
#[derive(Debug, Clone, Copy, Default)]
pub struct LinuxIoUringBackend;

impl IoBackend for LinuxIoUringBackend {
    fn is_available() -> bool {
        is_io_uring_available()
    }

    fn availability_reason() -> String {
        config::config_detail::io_uring_availability_reason()
    }

    fn sqpoll_fell_back() -> bool {
        sqpoll_fell_back()
    }
}

/// Reads an entire file using io_uring if available, falling back to standard I/O.
///
/// This is a convenience function for one-off file reads.
pub fn read_file<P: AsRef<std::path::Path>>(path: P) -> io::Result<Vec<u8>> {
    let factory = IoUringReaderFactory::default();
    let mut reader = factory.open(path.as_ref())?;
    reader.read_all()
}

/// Creates a writer from an existing file handle, respecting the io_uring policy.
///
/// This is the primary integration point for hot paths that open files
/// themselves (e.g., with `create_new` for atomic creation) but want to
/// leverage io_uring for the actual writes.
///
/// The `policy` parameter controls io_uring usage:
/// - `Auto`: use io_uring when available, fall back to standard I/O
/// - `Enabled`: require io_uring, return error if unavailable
/// - `Disabled`: always use standard buffered I/O
pub fn writer_from_file(
    file: File,
    buffer_capacity: usize,
    policy: crate::IoUringPolicy,
) -> io::Result<IoUringOrStdWriter> {
    writer_from_file_with_depth(file, buffer_capacity, policy, None)
}

/// Like [`writer_from_file`] but allows the caller to override the submission
/// queue depth.
///
/// `depth` corresponds to the `--io-uring-depth=N` CLI flag and overrides
/// [`IoUringConfig::sq_entries`] when `Some`. The value should already be
/// validated via [`crate::validate_io_uring_depth`].
pub fn writer_from_file_with_depth(
    file: File,
    buffer_capacity: usize,
    policy: crate::IoUringPolicy,
    depth: Option<u32>,
) -> io::Result<IoUringOrStdWriter> {
    let mut config = IoUringConfig::default();
    if let Some(d) = depth {
        config.sq_entries = d;
    }

    match policy {
        crate::IoUringPolicy::Auto => {
            if is_io_uring_available() {
                // Probe the per-thread ring; on setup failure `file` is still
                // ours and we fall back to standard I/O.
                match per_thread_ring::with_ring(|_| Ok(())) {
                    Ok(()) => {
                        return Ok(IoUringOrStdWriter::IoUring(IoUringWriter::with_ring(
                            file,
                            buffer_capacity,
                            config.sq_entries,
                            batching::NO_FIXED_FD,
                            config.register_buffers,
                            config.registered_buffer_count,
                        )));
                    }
                    Err(e) => {
                        logging::debug_log!(
                            Io,
                            1,
                            "io_uring per-thread ring probe failed, \
                             falling back to standard I/O for writer: {}",
                            e
                        );
                    }
                }
            }
            Ok(IoUringOrStdWriter::Std(
                crate::traits::StdFileWriter::from_file_with_capacity(file, buffer_capacity),
            ))
        }
        crate::IoUringPolicy::Enabled => {
            if !is_io_uring_available() {
                return Err(io::Error::new(
                    io::ErrorKind::Unsupported,
                    "io_uring requested via --io-uring but not available on this system",
                ));
            }
            per_thread_ring::with_ring(|_| Ok(()))?;
            Ok(IoUringOrStdWriter::IoUring(IoUringWriter::with_ring(
                file,
                buffer_capacity,
                config.sq_entries,
                batching::NO_FIXED_FD,
                config.register_buffers,
                config.registered_buffer_count,
            )))
        }
        crate::IoUringPolicy::Disabled => Ok(IoUringOrStdWriter::Std(
            crate::traits::StdFileWriter::from_file_with_capacity(file, buffer_capacity),
        )),
    }
}

/// Creates a reader from a file path, respecting the io_uring policy.
///
/// This is the read-side counterpart to [`writer_from_file`]. Used by the
/// sender/generator to read source files with io_uring when available.
///
/// The `policy` parameter controls io_uring usage:
/// - `Auto`: use io_uring when available, fall back to standard buffered I/O
/// - `Enabled`: require io_uring, return error if unavailable
/// - `Disabled`: always use standard buffered I/O
pub fn reader_from_path<P: AsRef<std::path::Path>>(
    path: P,
    policy: crate::IoUringPolicy,
) -> io::Result<IoUringOrStdReader> {
    reader_from_path_with_depth(path, policy, None)
}

/// Like [`reader_from_path`] but allows the caller to override the submission
/// queue depth.
///
/// `depth` corresponds to the `--io-uring-depth=N` CLI flag and overrides
/// [`IoUringConfig::sq_entries`] when `Some`. The value should already be
/// validated via [`crate::validate_io_uring_depth`].
pub fn reader_from_path_with_depth<P: AsRef<std::path::Path>>(
    path: P,
    policy: crate::IoUringPolicy,
    depth: Option<u32>,
) -> io::Result<IoUringOrStdReader> {
    let mut config = IoUringConfig::default();
    if let Some(d) = depth {
        config.sq_entries = d;
    }

    match policy {
        crate::IoUringPolicy::Auto => {
            if is_io_uring_available() {
                match IoUringReader::open(path.as_ref(), &config) {
                    Ok(r) => return Ok(IoUringOrStdReader::IoUring(r)),
                    Err(e) => {
                        logging::debug_log!(
                            Io,
                            1,
                            "io_uring reader open failed for {}, \
                             falling back to standard I/O: {}",
                            path.as_ref().display(),
                            e
                        );
                    }
                }
            }
            Ok(IoUringOrStdReader::Std(crate::traits::StdFileReader::open(
                path.as_ref(),
            )?))
        }
        crate::IoUringPolicy::Enabled => {
            if !is_io_uring_available() {
                return Err(io::Error::new(
                    io::ErrorKind::Unsupported,
                    "io_uring requested via --io-uring but not available on this system",
                ));
            }
            Ok(IoUringOrStdReader::IoUring(IoUringReader::open(
                path.as_ref(),
                &config,
            )?))
        }
        crate::IoUringPolicy::Disabled => Ok(IoUringOrStdReader::Std(
            crate::traits::StdFileReader::open(path.as_ref())?,
        )),
    }
}

/// Writes data to a file using io_uring if available, falling back to standard I/O.
///
/// This is a convenience function for one-off file writes.
pub fn write_file<P: AsRef<std::path::Path>>(path: P, data: &[u8]) -> io::Result<()> {
    let factory = IoUringWriterFactory::default();
    let mut writer = factory.create(path.as_ref())?;
    writer.write_all(data)?;
    writer.flush()?;
    Ok(())
}

/// Writes `data` to `path` through the io_uring registered-buffer write path.
///
/// Thin wrapper over the existing [`IoUringWriter`] + [`RegisteredBufferGroup`]
/// machinery: opens the destination via `IoUringWriter::create`, calls
/// `write_all` (which engages `IORING_OP_WRITE_FIXED` whenever the kernel
/// accepted buffer registration), then `flush` + `sync` to make the bytes
/// durable. No new submission helper is introduced; everything routes through
/// the same paths used by the live receiver disk thread.
///
/// Gated by `feature = "iouring-data-writes"` and `target_os = "linux"`. The
/// stub on every other configuration returns `io::ErrorKind::Unsupported` so
/// callers can dispatch via a single `cfg`-free call site and fall back when
/// the kernel path is not compiled in.
///
/// # Errors
///
/// Returns the underlying [`io::Error`] from ring construction, file creation,
/// SQE submission, or fsync. When io_uring is unavailable at runtime (kernel
/// pre-5.6, seccomp-blocked, or `io_uring_setup` rejection), the error is
/// surfaced as-is; the caller is expected to handle the fallback rather than
/// silently degrading to standard I/O.
#[cfg(all(target_os = "linux", feature = "iouring-data-writes"))]
pub fn write_file_with_io_uring(path: &std::path::Path, data: &[u8]) -> io::Result<()> {
    use crate::traits::FileWriter;

    let config = IoUringConfig::default();
    let mut writer = IoUringWriter::create(path, &config)?;
    writer.write_all(data)?;
    writer.flush()?;
    writer.sync()
}
