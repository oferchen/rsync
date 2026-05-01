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
//! stub module (`iocp_stub.rs`) provides the same public API with
//! `is_iocp_available()` always returning `false`.
//!
//! # Minimum file size
//!
//! Files smaller than [`IOCP_MIN_FILE_SIZE`] (64 KB) use standard buffered
//! I/O since the completion port setup overhead exceeds the async benefit.

mod completion_port;
pub mod config;
mod file_factory;
pub(crate) mod file_reader;
mod file_writer;
mod overlapped;
mod pump;

pub use config::{
    IOCP_MIN_FILE_SIZE, IocpConfig, iocp_availability_reason, is_iocp_available,
    skip_event_optimization_available,
};
pub use file_factory::{
    IocpOrStdReader, IocpOrStdWriter, IocpReaderFactory, IocpWriterFactory, reader_from_path,
    writer_from_file,
};
pub use file_reader::IocpReader;
pub use file_writer::IocpWriter;
pub use pump::{
    CompletionHandler, CompletionPump, IocpPumpConfig, oneshot_handler, post_completion,
};
