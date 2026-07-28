#![deny(unsafe_code)]
//! Server-side reader abstraction supporting plain and multiplex modes.
//!
//! Mirrors the writer module to handle incoming multiplexed messages.
//! When multiplex is active (protocol >= 23), this wrapper automatically
//! demultiplexes incoming messages, extracting MSG_DATA payloads.

mod counting;
mod multiplex;
mod server;

#[cfg(test)]
mod tests;

pub(crate) use counting::CountingReader;
pub use multiplex::RemoteExitError;
pub(crate) use multiplex::{DeletedRender, MultiplexReader};
pub use server::ServerReader;

/// Reports whether the next `Read` call is serviceable entirely from an
/// in-memory buffer, so no socket read - and therefore no blocking - will
/// occur.
///
/// The sender loop consults this before its pre-read flush. Upstream's
/// `perform_io()` (io.c:640-724) drains buffered output only while genuinely
/// waiting on input via `select()`; when the next request is already buffered
/// it returns immediately without touching output. Skipping the flush in that
/// case lets the writer coalesce per-file deltas up to its buffer bound,
/// matching upstream's `iobuf.out` batching (~24 files per socket write)
/// instead of forcing one write per file.
///
/// The default is the conservative `false` (assume a read may block, so flush
/// first); only the multiplex-capable [`ServerReader`] overrides it.
pub(crate) trait BufferedInputHint {
    /// Returns true when a subsequent read cannot block because its bytes are
    /// already buffered in user space.
    fn has_buffered_input(&self) -> bool {
        false
    }
}
