//! Sender/reader-side io_uring slurp wrapper (IUD-6 #2366).
//!
//! `IoUringFileReader` is a thin, opt-in entry point that mirrors the
//! `RegisteredBufferPool`-backed `READ_FIXED` path already implemented by
//! [`super::file_reader::IoUringReader`], but exposes a focused
//! `open` / `read_into` / `read_to_end` surface for the engine's basis-file
//! slurp dispatch.
//!
//! # Why a separate wrapper
//!
//! - The general [`IoUringReader`](super::file_reader::IoUringReader) is wired
//!   into the sender-source path via factories and configuration plumbing.
//!   The basis-file dispatch in `engine::concurrent_delta::strategy` only needs
//!   a "give me the whole file" entry point and benefits from a small,
//!   self-describing API.
//! - Keeping this wrapper behind the `iouring-data-reads` feature lets ops
//!   toggle the reader-side experiment independently of the writer-side
//!   `iouring-data-writes` prototype (#IUD-5) without affecting the always-on
//!   `io_uring` reader factory.
//!
//! # Reuse, never duplicate
//!
//! All buffer registration and `READ_FIXED` submission goes through
//! [`super::registered_buffers::RegisteredBufferGroup`] via the existing
//! [`IoUringReader::read_all_batched`] pipeline. This module adds no new
//! unsafe code.

use std::io;
use std::path::Path;

use crate::traits::FileReader;

use super::config::IoUringConfig;
use super::file_reader::IoUringReader;

/// Sender/reader-side io_uring file reader (IUD-6).
///
/// Opens a file read-only and submits `IORING_OP_READ_FIXED` against a
/// registered buffer pool via the shared [`IoUringReader`] machinery.
/// Single-purpose API for basis-file slurp paths; for sender-source
/// streaming through the `Read` trait, use
/// [`super::file_reader::IoUringReader`] directly.
pub struct IoUringFileReader {
    inner: IoUringReader,
    size: u64,
    position: u64,
}

impl IoUringFileReader {
    /// Opens `path` read-only and prepares a registered-buffer io_uring ring.
    ///
    /// The ring is configured with `register_buffers = true` so the slurp path
    /// uses `READ_FIXED` against the shared
    /// [`RegisteredBufferGroup`](super::registered_buffers::RegisteredBufferGroup)
    /// when the kernel honours registration. Registration failure transparently
    /// falls back to plain `IORING_OP_READ` (still io_uring, just without the
    /// pinned-page fast path) inside [`IoUringReader::read_all_batched`].
    ///
    /// # Errors
    ///
    /// Returns an error if the file cannot be opened or the io_uring instance
    /// cannot be constructed (e.g. seccomp-blocked, out of memory).
    pub fn open<P: AsRef<Path>>(path: P) -> io::Result<Self> {
        // Default config already requests buffer registration; we surface the
        // wrapper at this level so future tuning (page-aligned slurp sizing,
        // SQPOLL opt-in) lives in one place.
        let config = IoUringConfig::default();
        let inner = IoUringReader::open(path, &config)?;
        let size = inner.size();
        Ok(Self {
            inner,
            size,
            position: 0,
        })
    }

    /// Returns the total length of the underlying file in bytes.
    #[must_use]
    pub fn len(&self) -> u64 {
        self.size
    }

    /// Returns `true` when the file has zero length.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.size == 0
    }

    /// Reads up to `dst.len()` bytes at the current cursor position via
    /// `IORING_OP_READ_FIXED` (registered buffer) when available, advancing
    /// the cursor by the number of bytes read.
    ///
    /// Returns `Ok(0)` at end of file.
    ///
    /// # Errors
    ///
    /// Propagates the underlying io_uring submission or completion error.
    pub fn read_into(&mut self, dst: &mut [u8]) -> io::Result<usize> {
        if dst.is_empty() || self.position >= self.size {
            return Ok(0);
        }
        let n = self.inner.read_at(self.position, dst)?;
        self.position = self.position.saturating_add(n as u64);
        Ok(n)
    }

    /// Reads the entire file into a fresh `Vec<u8>` using the batched
    /// `READ_FIXED` slurp path.
    ///
    /// Internally delegates to
    /// [`IoUringReader::read_all_batched`](super::file_reader::IoUringReader::read_all_batched),
    /// which submits up to `sq_entries` registered-buffer reads per
    /// `submit_and_wait` and falls back to plain `IORING_OP_READ` when the
    /// kernel rejects buffer registration.
    ///
    /// # Errors
    ///
    /// Propagates the underlying io_uring submission, completion, or kernel
    /// errno surfaced during the batched read.
    pub fn read_to_end(&mut self) -> io::Result<Vec<u8>> {
        // Reset the public cursor so subsequent `read_into` calls cannot
        // race against the batched pipeline's own offset bookkeeping.
        self.position = self.size;
        self.inner.read_all_batched()
    }
}
