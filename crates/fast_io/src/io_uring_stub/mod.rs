//! Portable io_uring fallback for non-Linux platforms or when the feature is disabled.
//!
//! Provides the same public API as the real `io_uring` module but always falls
//! back to standard buffered I/O. The [`is_io_uring_available`] function always
//! returns `false`. This module is compiled when either:
//!
//! - The target OS is not Linux, or
//! - The `io_uring` cargo feature is not enabled
//!
//! All cross-platform plain-data types (configs, kernel UAPI constants, error
//! enums, telemetry structs) live in [`crate::io_uring_common`] so they
//! compile identically on every target. This module hosts only the
//! opaque-handle types and "always Unsupported" entry points that are unique
//! to the stub backend - which is the only thing the Linux backend cannot
//! share with us.
//!
//! Submodule layout mirrors [`crate::io_uring`] so cross-platform call sites
//! can import the same paths regardless of which backend is compiled.

#![allow(dead_code)]

/// Stub provided-buffer ring API mirroring the Linux backend.
pub mod buffer_ring;
/// Stub `IORING_OP_ASYNC_CANCEL` API mirroring the Linux backend.
pub mod cancel;
mod config;
mod disk_batch;
mod file_factory;
mod file_reader;
mod file_writer;
/// Stub `LINKAT` API mirroring the Linux backend.
pub mod linkat;
/// Stub linked-SQE chain API mirroring the Linux backend.
pub mod linked_chain;
/// Stub per-thread io_uring ring primitive mirroring the Linux backend
/// (IUR-3.a). [`with_ring`] always returns
/// [`std::io::ErrorKind::Unsupported`] on this platform.
pub mod per_thread_ring;
/// Stub registered-buffer API mirroring the Linux backend.
pub mod registered_buffers;
/// Stub `RENAMEAT2` API mirroring the Linux backend.
pub mod renameat2;
/// Stub `SEND_ZC` API mirroring the Linux backend.
pub mod send_zc;
/// Stub session ring-pool API mirroring the Linux backend.
pub mod session_pool;
/// Stub shared-ring API mirroring the Linux backend.
pub mod shared_ring;
#[cfg(unix)]
mod socket_factory;
#[cfg(unix)]
mod socket_reader;
#[cfg(unix)]
mod socket_writer;
/// Stub `IORING_OP_STATX` API mirroring the Linux backend.
pub mod statx;

#[cfg(test)]
mod tests;

pub use crate::io_uring_common::{
    ASYNC_CANCEL_FD_MIN_KERNEL, ASYNC_CANCEL_MIN_KERNEL, BgidAllocError, BufferRingConfig,
    BufferRingError, IORING_OP_ASYNC_CANCEL, IORING_OP_LINKAT, IORING_OP_RENAMEAT, IORING_OP_STATX,
    IoUringConfig, IoUringKernelInfo, LINKAT_MIN_KERNEL, OpTag, RENAME_EXCHANGE, RENAME_NOREPLACE,
    RENAME_WHITEOUT, RegisteredBufferStats, RegisteredBufferStatus, STATX_MIN_KERNEL,
    SharedCompletion, SharedRingConfig, buffer_id_from_cqe_flags,
};

pub use buffer_ring::{
    BgidAllocator, BgidSessionStats, BgidSnapshot, BufferRing, bgid_exhausted_count, bgid_inflight,
    bgid_peak_used, bgid_snapshot, pbuf_ring_supported,
};
pub use cancel::{CancelOutcome, cancel_all_by_fd, cancel_by_user_data};
pub use config::{StubIoUringBackend, config_detail, is_io_uring_available, sqpoll_fell_back};
pub use disk_batch::IoUringDiskBatch;
pub use file_factory::{
    IoUringOrStdReader, IoUringOrStdWriter, IoUringReaderFactory, IoUringWriterFactory, read_file,
    reader_from_path, reader_from_path_with_depth, write_file, writer_from_file,
    writer_from_file_with_depth,
};
pub use file_reader::IoUringReader;
pub use file_writer::IoUringWriter;
pub use linkat::{
    LinkAtArgs, build_linkat_sqe, build_linkat_sqe_unchecked, linkat_supported,
    submit_linkat_blocking,
};
pub use linked_chain::{CqeResult, LinkedChain, read_then_write};
pub use per_thread_ring::DEFAULT_RING_DEPTH as PER_THREAD_RING_DEPTH;
pub use registered_buffers::{RegisteredBufferGroup, RegisteredBufferSlot};
pub use renameat2::{
    RenameAt2Args, build_renameat2_sqe, build_renameat2_sqe_unchecked, renameat2_blocking,
    renameat2_supported,
};
pub use send_zc::is_supported as send_zc_supported;
#[cfg(feature = "iouring-send-zc")]
pub use send_zc::{SEND_ZC_DISPATCH_MIN_BYTES, ZeroCopySender};
pub use session_pool::{
    RingLease, SessionPoolConfig, SessionRingPool, ThreadLocalRingLease, ThreadLocalRingPool,
};
pub use shared_ring::SharedRing;
#[cfg(unix)]
pub use socket_factory::{
    IoUringOrStdSocketReader, IoUringOrStdSocketWriter, socket_reader_from_fd,
    socket_writer_from_fd,
};
#[cfg(unix)]
pub use socket_reader::IoUringSocketReader;
#[cfg(unix)]
pub use socket_writer::IoUringSocketWriter;
pub use statx::{
    StatxArgs, StatxResult, build_statx_sqe, build_statx_sqe_unchecked, statx_supported,
    submit_statx_batch, submit_statx_blocking,
};
