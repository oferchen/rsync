//! Windows I/O Completion Ports (IOCP) for async file I/O.
//!
//! This module provides high-performance file I/O using Windows IOCP,
//! which enables overlapped (async) reads and writes with completion
//! port notification. This is the Windows equivalent of Linux's io_uring.
//!
//! # Architecture
//!
//! Each reader/writer owns a dedicated completion port and uses
//! `FILE_FLAG_OVERLAPPED` for async operations. The completion port
//! dequeues completed I/O without polling or busy-waiting.
//!
//! # Runtime detection and fallback
//!
//! Availability is checked once via [`is_iocp_available`] and cached.
//! Factory types automatically fall back to standard buffered I/O when
//! IOCP is unavailable or for files too small to benefit from async I/O.
//!
//! On non-Windows platforms or when the `iocp` feature is disabled, the
//! stub module (`iocp_stub/`) provides the same public API with
//! `is_iocp_available()` always returning `false`.
//!
//! # Minimum file size
//!
//! Files smaller than [`IOCP_MIN_FILE_SIZE`] (64 KB) use standard buffered
//! I/O since the completion port setup overhead exceeds the async benefit.

mod completion_port;
/// IOCP configuration, availability detection, and caching.
pub mod config;
mod disk_batch;
/// Typed IOCP error variants for actionable failure handling.
pub mod error;
mod file_factory;
pub(crate) mod file_reader;
mod file_writer;
mod overlapped;
mod pump;
/// Windows Registered I/O (RIO) wrappers for opt-in high-throughput socket I/O.
pub mod rio;
/// IOCP-based async socket I/O via `WSARecv` / `WSASend`.
pub mod socket;
/// Windows `TransmitFile()` zero-copy file-to-socket primitive.
#[cfg(feature = "transmitfile")]
pub mod transmit_file;

pub use config::{
    IOCP_MIN_FILE_SIZE, IocpConfig, MAX_CONCURRENT_OPS, MIN_CONCURRENT_OPS,
    concurrent_ops_for_cpus, default_concurrent_ops, iocp_availability_reason, is_iocp_available,
    skip_event_optimization_available,
};
pub use disk_batch::{IocpDiskBatch, bounce_copies_avoided};
#[doc(hidden)]
pub use disk_batch::{
    clear_injected_completion_faults_for_test, clear_injected_write_error_for_test,
    inject_completion_faults_for_test, inject_next_write_error_for_test,
    reset_bounce_copies_avoided_for_test,
};
pub use error::IocpError;
pub use file_factory::{
    IocpOrStdReader, IocpOrStdWriter, IocpReaderFactory, IocpWriterFactory, reader_from_path,
    writer_from_file,
};
pub use file_reader::IocpReader;
pub use file_writer::IocpWriter;
pub use pump::{
    CompletionHandler, CompletionPump, IocpPumpConfig, oneshot_handler, post_completion,
};
pub use rio::{
    DEFAULT_RIO_POOL_BYTES, DEFAULT_RIO_SLOT_BYTES, RIO_ENV_VAR, RegisteredBuffer, RioBufferPool,
    RioCompletionQueue, RioFunctions, RioMode, rio_create_request_queue, rio_enabled_from_env,
    rio_notify, rio_recv, rio_send, try_init_rio,
};
#[cfg(feature = "transmitfile")]
pub use transmit_file::{TRANSMIT_FILE_MAX_BYTES, try_transmit_file};
