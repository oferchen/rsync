//! Batched IOCP disk writer for multi-file commit operations.
//!
//! Provides [`IocpDiskBatch`] which mirrors the surface of
//! [`crate::io_uring::IoUringDiskBatch`] so the disk-commit thread can use
//! the same calling convention on Windows as it does on Linux.
//!
//! # Architecture
//!
//! A single [`super::completion_port::CompletionPort`] is created once and
//! reused across every file processed by the batch writer. For each active
//! file the batch:
//!
//! 1. Reopens the caller's file handle with `FILE_FLAG_OVERLAPPED` so it can
//!    be associated with the completion port. The original `File` is held to
//!    preserve its lifetime and is returned by [`IocpDiskBatch::commit_file`]
//!    so callers can rename/finalize it.
//! 2. Buffers writes internally up to a configurable chunk size that matches
//!    upstream rsync's `wf_writeBufSize` (256 KB).
//! 3. On flush, splits the buffer into chunks and submits up to
//!    `concurrent_ops` overlapped `WriteFile` calls in flight, draining the
//!    completion port via `GetQueuedCompletionStatusEx` between submissions.
//!
//! This is the Windows analogue of io_uring's submission-queue batching: a
//! single completion port amortizes the per-file association overhead and
//! `GetQueuedCompletionStatusEx` reaps multiple completions per syscall.
//!
//! # Cross-platform
//!
//! The real implementation lives behind `#[cfg(all(target_os = "windows",
//! feature = "iocp"))]`. The non-Windows stub (`crate::iocp_stub`) provides
//! the same public surface with `Unsupported` errors so the crate compiles on
//! Linux and macOS. [`IocpDiskBatch::try_new`] returns `None` on every
//! platform where IOCP is unavailable so callers can fall back transparently.
//!
//! # Submodule layout
//!
//! - `buffer`: accumulation-buffer enum and the bounce-copy telemetry counter.
//! - `writer`: `WriteFile` dispatch, overlapped-handle lifecycle, batched submission.
//! - `completion`: `GetQueuedCompletionStatusEx` drain, NTSTATUS mapping, test fault injector.
//!
//! # Upstream Reference
//!
//! Upstream rsync 3.4.1 has no IOCP code path; the batched-write surface is
//! defined for parity with `crates/fast_io/src/io_uring/disk_batch.rs`
//! (PR #1086) so the disk-commit thread can use one calling convention on
//! both platforms.

mod buffer;
mod completion;
mod writer;

use std::fs::File;
use std::io::{self, Write};
use std::os::windows::io::AsRawHandle;

use windows_sys::Win32::Foundation::{HANDLE, TRUE};
use windows_sys::Win32::Storage::FileSystem::FlushFileBuffers;

use super::completion_port::CompletionPort;
use super::config::IocpConfig;
use crate::page_aligned::{PageAlignedBuffer, round_up_to_page};

use buffer::BatchBuffer;
use writer::{close_overlapped_handle, reopen_overlapped, submit_write_batch};

pub use buffer::{bounce_copies_avoided, reset_bounce_copies_avoided_for_test};
pub use completion::{
    clear_injected_completion_faults_for_test, clear_injected_write_error_for_test,
    inject_completion_faults_for_test, inject_next_write_error_for_test,
};

/// Default write buffer capacity matching upstream's `wf_writeBufSize`
/// (`fileio.c:161` -> `WRITE_SIZE * 8` = 256 KB).
const DEFAULT_BUFFER_CAPACITY: usize = 256 * 1024;

/// Batched IOCP disk writer for the disk commit phase.
///
/// Owns a single [`CompletionPort`] reused across every file it processes.
/// When a new file is started via [`begin_file`](Self::begin_file), the
/// previous file's data is flushed, its overlapped handle is closed, and the
/// new file is associated with the port using a fresh per-file completion
/// key.
///
/// # Buffer Alignment
///
/// When [`IocpConfig::unbuffered`] is set the writer allocates its
/// accumulation buffer through [`PageAlignedBuffer`], and each `WriteFile`
/// submission uses [`crate::iocp::overlapped::OverlappedOp::new_write_aligned`]
/// so the per-chunk pinned buffer is also page-aligned. The handle is
/// reopened with `FILE_FLAG_NO_BUFFERING` (and optionally
/// `FILE_FLAG_WRITE_THROUGH`) so the kernel can issue the I/O directly from
/// the caller's buffer without the alignment-fixup bounce copy that
/// misaligned submissions force.
///
/// # Thread Safety
///
/// Not `Send` or `Sync` - designed for single-threaded use on the dedicated
/// disk commit thread, mirroring [`crate::io_uring::IoUringDiskBatch`].
pub struct IocpDiskBatch {
    port: CompletionPort,
    config: IocpConfig,
    /// Currently active file, if any.
    current_file: Option<ActiveFile>,
    /// Reusable write buffer.
    buffer: BatchBuffer,
    /// Current write position in the buffer.
    buffer_pos: usize,
    /// Per-file completion key counter so each file has a distinct key when
    /// dropped associations linger in the port's queue.
    next_completion_key: usize,
}

/// Tracks the currently active file in the batch writer.
struct ActiveFile {
    /// Caller-owned `File`. Held for lifetime parity with the io_uring
    /// version - returned by [`IocpDiskBatch::commit_file`] so callers can
    /// rename/finalize via the same handle they passed in.
    file: File,
    /// Handle reopened with `FILE_FLAG_OVERLAPPED` for IOCP submission. Owned
    /// by the active-file slot and closed when the file is committed or a
    /// subsequent `begin_file` rotates it out.
    overlapped_handle: HANDLE,
    /// Cumulative bytes successfully written and reaped from the port.
    bytes_written: u64,
    /// Per-file completion key associated with the port.
    #[allow(dead_code)] // REASON: kept for diagnostic parity with io_uring fixed-fd slot id
    completion_key: usize,
}

impl IocpDiskBatch {
    /// Creates a new batched IOCP disk writer with the given configuration.
    ///
    /// Allocates a single completion port that will be reused across all
    /// file operations. Returns `Err` if the port cannot be created.
    pub fn new(config: &IocpConfig) -> io::Result<Self> {
        // One concurrent worker thread is enough: the batch is single-threaded
        // and drains its own completions inline.
        let port = CompletionPort::new(1)?;
        let buffer_capacity = config.buffer_size.max(DEFAULT_BUFFER_CAPACITY);
        let buffer = if config.unbuffered {
            // Round up so the underlying allocation is a clean page multiple.
            // PageAlignedBuffer's capacity will report the rounded value,
            // which is what the chunker uses to size each WriteFile.
            BatchBuffer::PageAligned(PageAlignedBuffer::new(round_up_to_page(buffer_capacity)))
        } else {
            BatchBuffer::Vec(vec![0u8; buffer_capacity])
        };
        Ok(Self {
            port,
            config: config.clone(),
            current_file: None,
            buffer,
            buffer_pos: 0,
            next_completion_key: 1,
        })
    }

    /// Returns whether the accumulation buffer is page-aligned.
    ///
    /// Always `true` when constructed with `IocpConfig::unbuffered = true`.
    /// Useful for tests and runtime status output.
    #[must_use]
    pub fn buffer_is_page_aligned(&self) -> bool {
        self.buffer.is_page_aligned()
    }

    /// Attempts to create a batched IOCP disk writer, returning `None` if
    /// IOCP is unavailable or port creation fails.
    ///
    /// This is the recommended constructor for production use - callers
    /// should fall back to standard buffered I/O when `None` is returned.
    pub fn try_new(config: &IocpConfig) -> Option<Self> {
        if !super::config::is_iocp_available() {
            return None;
        }
        Self::new(config).ok()
    }

    /// Begins a new file for writing.
    ///
    /// Flushes any buffered data for the previous file, closes its overlapped
    /// handle, and associates the new file's handle with the completion port.
    /// The caller's `File` is reopened internally with `FILE_FLAG_OVERLAPPED`
    /// (the original handle is preserved so it can be returned by
    /// [`Self::commit_file`]).
    ///
    /// # Errors
    ///
    /// Returns an error if flushing the previous file fails, if `ReOpenFile`
    /// cannot acquire an overlapped handle, or if the port association fails.
    pub fn begin_file(&mut self, file: File) -> io::Result<()> {
        // Flush and finalize the previous file before rotating in the new
        // one. Mirrors IoUringDiskBatch::begin_file ordering.
        self.flush_current()?;
        self.finalize_current_file();

        let raw_handle = file.as_raw_handle() as HANDLE;
        let overlapped_handle = reopen_overlapped(raw_handle, &self.config)?;

        let key = self.next_completion_key;
        self.next_completion_key = self.next_completion_key.wrapping_add(1).max(1);

        if let Err(e) = self.port.associate(overlapped_handle, key) {
            close_overlapped_handle(overlapped_handle);
            return Err(e);
        }

        self.current_file = Some(ActiveFile {
            file,
            overlapped_handle,
            bytes_written: 0,
            completion_key: key,
        });
        Ok(())
    }

    /// Writes data to the current file.
    ///
    /// Data is buffered internally and flushed in batched overlapped writes
    /// when the buffer fills or [`flush`](Self::flush) is called.
    ///
    /// # Errors
    ///
    /// Returns an error if no file is active or if a flush fails.
    pub fn write_data(&mut self, data: &[u8]) -> io::Result<()> {
        if self.current_file.is_none() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "no file active in batched IOCP disk writer",
            ));
        }

        let mut offset = 0;
        let capacity = self.buffer.len();
        while offset < data.len() {
            let available = capacity - self.buffer_pos;
            let to_copy = available.min(data.len() - offset);

            if to_copy > 0 {
                let pos = self.buffer_pos;
                self.buffer.as_mut_slice()[pos..pos + to_copy]
                    .copy_from_slice(&data[offset..offset + to_copy]);
                self.buffer_pos += to_copy;
                offset += to_copy;
            }

            if self.buffer_pos == capacity {
                self.flush_current()?;
            }
        }
        Ok(())
    }

    /// Flushes buffered data to the current file using batched overlapped
    /// writes.
    ///
    /// # Errors
    ///
    /// Returns an error if no file is active or any submitted write fails.
    pub fn flush(&mut self) -> io::Result<()> {
        self.flush_current()
    }

    /// Commits the current file: flushes data and optionally calls
    /// `FlushFileBuffers` (the Windows analogue of `fsync`).
    ///
    /// After this call, the file is no longer active. The caller receives
    /// the original `File` handle for rename/metadata operations along with
    /// the total bytes written.
    ///
    /// # Errors
    ///
    /// Returns an error if flushing or `FlushFileBuffers` fails.
    pub fn commit_file(&mut self, do_fsync: bool) -> io::Result<(File, u64)> {
        self.flush_current()?;

        let active = self.current_file.take().ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "no file active in batched IOCP disk writer",
            )
        })?;

        let bytes_written = active.bytes_written;

        if do_fsync {
            // Flush through the original handle - that is what the caller
            // will use post-commit, so flushing it removes any ambiguity
            // about write-behind caching across handles.
            let raw = active.file.as_raw_handle() as HANDLE;
            // SAFETY: `raw` came from a live `File` we still own.
            #[allow(unsafe_code)]
            let ok = unsafe { FlushFileBuffers(raw) };
            if ok != TRUE {
                let err = io::Error::last_os_error();
                close_overlapped_handle(active.overlapped_handle);
                return Err(err);
            }
        }

        close_overlapped_handle(active.overlapped_handle);
        Ok((active.file, bytes_written))
    }

    /// Returns the bytes successfully written to the current file (drained).
    ///
    /// Pending buffer contents are not included; use
    /// [`Self::bytes_written_with_pending`] for that view.
    #[must_use]
    pub fn bytes_written(&self) -> u64 {
        self.current_file.as_ref().map_or(0, |f| f.bytes_written)
    }

    /// Returns the total bytes including unflushed buffer content.
    #[must_use]
    pub fn bytes_written_with_pending(&self) -> u64 {
        self.current_file
            .as_ref()
            .map_or(0, |f| f.bytes_written + self.buffer_pos as u64)
    }

    /// Flushes the internal buffer to the current file via batched overlapped
    /// writes.
    ///
    /// On a mid-flush failure (synchronous `WriteFile` error, drained
    /// completion that reports an NTSTATUS failure, or test-injected
    /// `ERROR_DISK_FULL`), any chunks that did reach the kernel before the
    /// fault are credited to `active.bytes_written` and the in-memory buffer
    /// is cleared so the `Drop` retry does not resubmit data that already
    /// landed. The error is then propagated to the caller unchanged.
    fn flush_current(&mut self) -> io::Result<()> {
        if self.buffer_pos == 0 {
            return Ok(());
        }

        let active = self.current_file.as_mut().ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "no file active in batched IOCP disk writer",
            )
        })?;

        let len = self.buffer_pos;
        let base_offset = active.bytes_written;
        let chunk_size = self.config.buffer_size.max(1);
        let max_in_flight = self.config.concurrent_ops.max(1) as usize;
        let use_aligned = self.config.unbuffered;

        let mut written = 0usize;
        let result = submit_write_batch(
            &self.port,
            active.overlapped_handle,
            &self.buffer.as_slice()[..len],
            base_offset,
            chunk_size,
            max_in_flight,
            use_aligned,
            &mut written,
        );

        // Always credit the partial progress before deciding the fate of the
        // pending buffer: a mid-flush failure must not double-write chunks
        // the kernel already accepted.
        active.bytes_written += written as u64;
        self.buffer_pos = 0;
        result
    }

    /// Drops the current file without returning it. Used when rotating in a
    /// new file mid-stream (the previous file's data has already been
    /// flushed by the caller of `flush_current`).
    fn finalize_current_file(&mut self) {
        if let Some(active) = self.current_file.take() {
            close_overlapped_handle(active.overlapped_handle);
            // `active.file` drops here, closing the original handle.
        }
    }
}

impl Write for IocpDiskBatch {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.write_data(buf)?;
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        self.flush_current()
    }
}

impl Drop for IocpDiskBatch {
    fn drop(&mut self) {
        // Best-effort flush + cleanup. Errors on drop are swallowed because
        // the consumer is no longer interested in I/O outcomes.
        let _ = self.flush_current();
        self.finalize_current_file();
    }
}

#[cfg(test)]
mod tests;
