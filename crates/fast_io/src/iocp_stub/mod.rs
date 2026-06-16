//! Portable IOCP fallback for non-Windows platforms or when the feature is disabled.
//!
//! Provides the same public API as the real `iocp` module but always falls
//! back to standard buffered I/O. The [`is_iocp_available`] function always
//! returns `false`. This module is compiled when either:
//!
//! - The target OS is not Windows, or
//! - The `iocp` cargo feature is not enabled
//!
//! All factory types ([`IocpReaderFactory`], [`IocpWriterFactory`]) produce
//! `Std` variants directly. The stub types ([`IocpReader`], [`IocpWriter`])
//! cannot be constructed and exist only for enum variant completeness.
//!
//! Submodule layout mirrors [`crate::iocp`] so cross-platform call sites can
//! import the same paths regardless of which backend is compiled.

#![allow(dead_code)]

/// Stub IOCP configuration and availability probes mirroring the Windows backend.
pub mod config;
mod disk_batch;
/// Typed IOCP error variants mirroring the Windows backend.
pub mod error;
mod file_factory;
mod file_reader;
mod file_writer;
mod pump;
/// Cross-platform stub for the Windows-only `iocp::rio` module.
pub mod rio;
/// Cross-platform stub for the Windows-only `iocp::socket` module.
pub mod socket;

#[cfg(test)]
mod tests;

pub use config::{
    IOCP_MIN_FILE_SIZE, IocpConfig, MAX_CONCURRENT_OPS, MIN_CONCURRENT_OPS,
    concurrent_ops_for_cpus, default_concurrent_ops, iocp_availability_reason, is_iocp_available,
    skip_event_optimization_available,
};
#[doc(hidden)]
pub use disk_batch::reset_bounce_copies_avoided_for_test;
pub use disk_batch::{IocpDiskBatch, bounce_copies_avoided};
pub use error::IocpError;
pub use file_factory::{
    IocpOrStdReader, IocpOrStdWriter, IocpReaderFactory, IocpWriterFactory, reader_from_path,
    writer_from_file,
};
pub use file_reader::IocpReader;
pub use file_writer::IocpWriter;
pub use pump::{CompletionHandler, CompletionPump, IocpPumpConfig, oneshot_handler};
pub use rio::{
    DEFAULT_RIO_POOL_BYTES, DEFAULT_RIO_SLOT_BYTES, RIO_ENV_VAR, RegisteredBuffer, RioBufferPool,
    RioCompletionQueue, RioFunctions, RioMode, rio_enabled_from_env, try_init_rio,
};
