//! Streaming message sink and supporting types.
//!
//! This module re-exports [`MessageSink`], [`LineModeGuard`], and
//! [`TryMapWriterError`] - the primary types for rendering
//! [`core::message::Message`] values into arbitrary [`std::io::Write`] targets.

mod guard;
mod message_sink;
mod try_map_writer_error;

pub use guard::LineModeGuard;
pub use message_sink::MessageSink;
pub use try_map_writer_error::TryMapWriterError;

#[cfg(test)]
mod tests;
