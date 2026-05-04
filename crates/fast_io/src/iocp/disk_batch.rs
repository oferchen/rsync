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
//! # Upstream Reference
//!
//! Upstream rsync 3.4.1 has no IOCP code path; the batched-write surface is
//! defined for parity with `crates/fast_io/src/io_uring/disk_batch.rs`
//! (PR #1086) so the disk-commit thread can use one calling convention on
//! both platforms.

use std::fs::File;
use std::io::{self, Write};
use std::os::windows::io::AsRawHandle;
use std::pin::Pin;

use windows_sys::Win32::Foundation::{
    CloseHandle, ERROR_HANDLE_EOF, FALSE, HANDLE, INVALID_HANDLE_VALUE, TRUE, WAIT_TIMEOUT,
};
use windows_sys::Win32::Storage::FileSystem::{
    FILE_FLAG_OVERLAPPED, FILE_GENERIC_WRITE, FILE_SHARE_READ, FlushFileBuffers, ReOpenFile,
    WriteFile,
};
use windows_sys::Win32::System::IO::{GetQueuedCompletionStatusEx, OVERLAPPED, OVERLAPPED_ENTRY};

use super::completion_port::CompletionPort;
use super::config::IocpConfig;
use super::overlapped::OverlappedOp;

/// Default write buffer capacity matching upstream's `wf_writeBufSize`
/// (`fileio.c:161` -> `WRITE_SIZE * 8` = 256 KB).
const DEFAULT_BUFFER_CAPACITY: usize = 256 * 1024;

/// Maximum entries dequeued by a single `GetQueuedCompletionStatusEx` call.
///
/// Matches the io_uring side's CQE batch sizing so both backends use the
/// same drain granularity.
const COMPLETION_DRAIN_BATCH: usize = 64;

/// Wait timeout for completion drains, in milliseconds. The disk batch
/// always knows how many completions are outstanding so it waits
/// indefinitely (`u32::MAX`) until every submitted write has been reaped.
const DRAIN_TIMEOUT_MS: u32 = u32::MAX;

/// Batched IOCP disk writer for the disk commit phase.
///
/// Owns a single [`CompletionPort`] reused across every file it processes.
/// When a new file is started via [`begin_file`](Self::begin_file), the
/// previous file's data is flushed, its overlapped handle is closed, and the
/// new file is associated with the port using a fresh per-file completion
/// key.
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
    buffer: Vec<u8>,
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
        Ok(Self {
            port,
            config: config.clone(),
            current_file: None,
            buffer: vec![0u8; buffer_capacity],
            buffer_pos: 0,
            next_completion_key: 1,
        })
    }

    /// Attempts to create a batched IOCP disk writer, returning `None` if
    /// IOCP is unavailable or port creation fails.
    ///
    /// This is the recommended constructor for production use - callers
    /// should fall back to standard buffered I/O when `None` is returned.
    #[must_use]
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
        let overlapped_handle = reopen_overlapped(raw_handle)?;

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
        while offset < data.len() {
            let available = self.buffer.len() - self.buffer_pos;
            let to_copy = available.min(data.len() - offset);

            if to_copy > 0 {
                self.buffer[self.buffer_pos..self.buffer_pos + to_copy]
                    .copy_from_slice(&data[offset..offset + to_copy]);
                self.buffer_pos += to_copy;
                offset += to_copy;
            }

            if self.buffer_pos == self.buffer.len() {
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

        let written = submit_write_batch(
            &self.port,
            active.overlapped_handle,
            &self.buffer[..len],
            base_offset,
            chunk_size,
            max_in_flight,
        )?;

        active.bytes_written += written as u64;
        self.buffer_pos = 0;
        Ok(())
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

/// Reopens an existing file handle with `FILE_FLAG_OVERLAPPED` so it can be
/// associated with a completion port.
///
/// Mirrors the Microsoft-documented pattern for converting an
/// already-opened handle into one that supports overlapped I/O without
/// reopening the path. The returned handle must be closed with
/// `CloseHandle` once no longer needed.
fn reopen_overlapped(handle: HANDLE) -> io::Result<HANDLE> {
    // SAFETY: `handle` is borrowed from the caller's live File for the
    // duration of the call. ReOpenFile returns a new handle with the
    // requested access/share/flag combination; failure is signalled by
    // INVALID_HANDLE_VALUE per Microsoft docs.
    #[allow(unsafe_code)]
    let new_handle = unsafe {
        ReOpenFile(
            handle,
            FILE_GENERIC_WRITE,
            FILE_SHARE_READ,
            FILE_FLAG_OVERLAPPED,
        )
    };

    if new_handle == INVALID_HANDLE_VALUE {
        return Err(io::Error::last_os_error());
    }

    Ok(new_handle)
}

/// Closes a handle obtained from [`reopen_overlapped`].
fn close_overlapped_handle(handle: HANDLE) {
    if handle.is_null() || handle == INVALID_HANDLE_VALUE {
        return;
    }
    // SAFETY: `handle` was obtained from ReOpenFile and is still owned by
    // this call site (it has been removed from the active-file slot).
    #[allow(unsafe_code)]
    unsafe {
        CloseHandle(handle);
    }
}

/// Submits a batch of overlapped writes from `data` and drains every
/// completion before returning.
///
/// Splits `data` into `chunk_size`-sized pieces, keeps up to
/// `max_in_flight` outstanding `WriteFile` calls at once, and uses
/// `GetQueuedCompletionStatusEx` to reap completed entries in batches.
/// Short writes inside a chunk are resubmitted at the appropriate offset
/// until the chunk is fully written.
fn submit_write_batch(
    port: &CompletionPort,
    handle: HANDLE,
    data: &[u8],
    base_offset: u64,
    chunk_size: usize,
    max_in_flight: usize,
) -> io::Result<usize> {
    if data.is_empty() {
        return Ok(0);
    }

    let total = data.len();
    let mut next_chunk_start = 0usize;
    let mut total_written = 0usize;
    let mut in_flight: Vec<Pin<Box<OverlappedOp>>> = Vec::with_capacity(max_in_flight);

    while next_chunk_start < total || !in_flight.is_empty() {
        // Fill the in-flight queue up to the configured limit.
        while in_flight.len() < max_in_flight && next_chunk_start < total {
            let len = chunk_size.min(total - next_chunk_start);
            let chunk = &data[next_chunk_start..next_chunk_start + len];
            let offset = base_offset + next_chunk_start as u64;
            let op = submit_one_write(handle, offset, chunk)?;
            in_flight.push(op);
            next_chunk_start += len;
        }

        if in_flight.is_empty() {
            break;
        }

        // Reap at least one completion. The drain returns a list of bytes
        // transferred per completed OVERLAPPED pointer; map those back to
        // the in-flight queue and remove completed entries.
        let completions = drain_completions(port, in_flight.len())?;
        let mut resubmissions: Vec<(u64, Vec<u8>)> = Vec::new();
        let mut zero_byte_completion = false;

        in_flight.retain_mut(|op| {
            let ptr = pinned_overlapped_addr(op);
            if let Some(transferred) = completions
                .iter()
                .find_map(|(p, n)| if *p == ptr { Some(*n) } else { None })
            {
                let chunk_len = op.buffer.len();
                if transferred == chunk_len {
                    total_written += transferred;
                    false
                } else if transferred == 0 {
                    zero_byte_completion = true;
                    false
                } else {
                    // Short write: resubmit the unwritten tail at the
                    // appropriate offset.
                    total_written += transferred;
                    let remaining = op.buffer[transferred..].to_vec();
                    let original_offset = read_offset(&op.overlapped);
                    let new_offset = original_offset + transferred as u64;
                    resubmissions.push((new_offset, remaining));
                    false
                }
            } else {
                true
            }
        });

        if zero_byte_completion {
            return Err(io::Error::new(
                io::ErrorKind::WriteZero,
                "overlapped write returned zero bytes",
            ));
        }

        for (offset, remaining) in resubmissions {
            let op = submit_one_write(handle, offset, &remaining)?;
            in_flight.push(op);
        }
    }

    Ok(total_written)
}

/// Returns the address of the OVERLAPPED structure pinned inside the boxed op.
fn pinned_overlapped_addr(op: &Pin<Box<OverlappedOp>>) -> usize {
    (&op.overlapped as *const OVERLAPPED) as usize
}

/// Reads the 64-bit offset out of a populated OVERLAPPED.
fn read_offset(overlapped: &OVERLAPPED) -> u64 {
    // SAFETY: The OVERLAPPED was populated by OverlappedOp; reading its
    // offset union members is well-defined for entries the kernel has
    // already returned (or that we just initialised).
    #[allow(unsafe_code)]
    unsafe {
        let lo = overlapped.Anonymous.Anonymous.Offset as u64;
        let hi = overlapped.Anonymous.Anonymous.OffsetHigh as u64;
        (hi << 32) | lo
    }
}

/// Issues a single `WriteFile` overlapped submission and returns the pinned
/// op. Synchronous-success completions still post to the completion port
/// because we never set `FILE_SKIP_COMPLETION_PORT_ON_SUCCESS`, so the
/// drain loop reaps them uniformly.
fn submit_one_write(
    handle: HANDLE,
    offset: u64,
    data: &[u8],
) -> io::Result<Pin<Box<OverlappedOp>>> {
    let mut op = OverlappedOp::new_write(offset, data);
    let overlapped_ptr = op.as_overlapped_ptr();

    let mut bytes_written: u32 = 0;

    // SAFETY: `handle` is a valid overlapped file handle owned by the
    // active-file slot. The op buffer and OVERLAPPED are pinned for the
    // duration of the asynchronous call.
    #[allow(unsafe_code)]
    let success = unsafe {
        WriteFile(
            handle,
            op.buffer.as_ptr().cast(),
            op.buffer.len() as u32,
            &mut bytes_written,
            overlapped_ptr,
        )
    };

    if success == TRUE {
        // Synchronous success still queues a completion packet because we
        // do not opt into FILE_SKIP_COMPLETION_PORT_ON_SUCCESS; drop into
        // the drain loop just like an ERROR_IO_PENDING.
        return Ok(op);
    }

    let err = io::Error::last_os_error();
    // ERROR_IO_PENDING (997) is the documented "queued" code; any other
    // error is fatal for this submission.
    if err.raw_os_error() != Some(997) {
        return Err(err);
    }

    Ok(op)
}

/// Drains up to `max` completion entries from the port using
/// `GetQueuedCompletionStatusEx` and returns
/// `(overlapped_address, bytes_transferred)` pairs.
fn drain_completions(port: &CompletionPort, max: usize) -> io::Result<Vec<(usize, usize)>> {
    let batch = max.clamp(1, COMPLETION_DRAIN_BATCH);
    let mut entries: Vec<OVERLAPPED_ENTRY> = vec![zeroed_entry(); batch];

    loop {
        let mut removed: u32 = 0;
        // SAFETY: `port.handle()` is owned by `port` and lives for the
        // duration of the call; `entries` backs `batch` slots.
        #[allow(unsafe_code)]
        let ok = unsafe {
            GetQueuedCompletionStatusEx(
                port.handle(),
                entries.as_mut_ptr(),
                batch as u32,
                &mut removed,
                DRAIN_TIMEOUT_MS,
                FALSE,
            )
        };

        if ok == FALSE {
            let err = io::Error::last_os_error();
            // Spurious wake without entries: retry.
            if matches!(err.raw_os_error(), Some(c) if c as u32 == WAIT_TIMEOUT) {
                continue;
            }
            return Err(err);
        }

        let mut out = Vec::with_capacity(removed as usize);
        for entry in entries.iter().take(removed as usize) {
            let overlapped_ptr = entry.lpOverlapped;
            if overlapped_ptr.is_null() {
                continue;
            }
            // SAFETY: entry.lpOverlapped points at the OVERLAPPED structure
            // we submitted; the surrounding pinned op is still alive in
            // the in-flight queue, so reading the Internal field is sound.
            #[allow(unsafe_code)]
            let internal = unsafe { (*overlapped_ptr).Internal };
            if internal != 0 {
                let dos_error = ntstatus_to_dos_error(internal as u32);
                if dos_error == ERROR_HANDLE_EOF {
                    return Err(io::Error::from(io::ErrorKind::UnexpectedEof));
                }
                return Err(io::Error::from_raw_os_error(dos_error as i32));
            }
            out.push((
                overlapped_ptr as usize,
                entry.dwNumberOfBytesTransferred as usize,
            ));
        }
        return Ok(out);
    }
}

/// Translates the small set of NTSTATUS codes that overlapped file I/O can
/// produce into Win32 DOS error codes.
fn ntstatus_to_dos_error(status: u32) -> u32 {
    match status {
        0xC000_0011 => ERROR_HANDLE_EOF, // STATUS_END_OF_FILE
        0xC000_0120 => 995,              // STATUS_CANCELLED
        0xC000_009A => 1450,             // STATUS_INSUFFICIENT_RESOURCES
        0xC000_00B5 => 121,              // STATUS_IO_TIMEOUT
        other => other,
    }
}

/// Constructs a zeroed `OVERLAPPED_ENTRY` for batch dequeues.
fn zeroed_entry() -> OVERLAPPED_ENTRY {
    // SAFETY: OVERLAPPED_ENTRY is plain-old-data and valid when zeroed.
    #[allow(unsafe_code)]
    unsafe {
        std::mem::zeroed()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::tempdir;
    use windows_sys::Win32::Storage::FileSystem::{
        CREATE_ALWAYS, CreateFileW, FILE_ATTRIBUTE_NORMAL, FILE_GENERIC_WRITE, FILE_SHARE_DELETE,
        FILE_SHARE_READ, FILE_SHARE_WRITE,
    };

    fn to_wide(path: &std::path::Path) -> Vec<u16> {
        use std::os::windows::ffi::OsStrExt;
        path.as_os_str()
            .encode_wide()
            .chain(std::iter::once(0))
            .collect()
    }

    fn open_writable(path: &std::path::Path) -> File {
        let wide = to_wide(path);
        // SAFETY: Standard Win32 open: zero-terminated wide string, generic
        // write, create-always. The handle permits shared read/write/delete
        // so that `ReOpenFile` (called from `begin_file`) can acquire a
        // second overlapped write handle without ERROR_SHARING_VIOLATION,
        // and so the enclosing tempdir can be cleaned up. The returned
        // handle is wrapped into a std::fs::File via FromRawHandle so Drop
        // closes it.
        #[allow(unsafe_code)]
        let handle = unsafe {
            CreateFileW(
                wide.as_ptr(),
                FILE_GENERIC_WRITE,
                FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
                std::ptr::null(),
                CREATE_ALWAYS,
                FILE_ATTRIBUTE_NORMAL,
                std::ptr::null_mut(),
            )
        };
        assert_ne!(
            handle,
            INVALID_HANDLE_VALUE,
            "CreateFileW failed: {}",
            io::Error::last_os_error()
        );
        // SAFETY: `handle` is a fresh, exclusively-owned handle.
        #[allow(unsafe_code)]
        unsafe {
            use std::os::windows::io::FromRawHandle;
            File::from_raw_handle(handle as *mut std::ffi::c_void)
        }
    }

    #[test]
    fn try_new_returns_some_on_windows() {
        let config = IocpConfig::default();
        let batch = IocpDiskBatch::try_new(&config);
        assert!(
            batch.is_some(),
            "IOCP must be available on every supported Windows host"
        );
    }

    #[test]
    fn write_without_active_file_errors() {
        let config = IocpConfig::default();
        let mut batch = IocpDiskBatch::new(&config).unwrap();
        let result = batch.write_data(b"hello");
        assert!(result.is_err());
        assert_eq!(result.unwrap_err().kind(), io::ErrorKind::InvalidInput);
    }

    #[test]
    fn commit_without_active_file_errors() {
        let config = IocpConfig::default();
        let mut batch = IocpDiskBatch::new(&config).unwrap();
        let result = batch.commit_file(false);
        assert!(result.is_err());
        assert_eq!(result.unwrap_err().kind(), io::ErrorKind::InvalidInput);
    }

    #[test]
    fn bytes_written_accessors_default_to_zero() {
        let config = IocpConfig::default();
        let batch = IocpDiskBatch::new(&config).unwrap();
        assert_eq!(batch.bytes_written(), 0);
        assert_eq!(batch.bytes_written_with_pending(), 0);
    }

    #[test]
    fn single_file_write_and_commit() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("single.bin");
        let file = open_writable(&path);

        let mut batch = IocpDiskBatch::new(&IocpConfig::default()).unwrap();
        batch.begin_file(file).unwrap();

        let payload = b"hello iocp disk batch";
        batch.write_data(payload).unwrap();
        let (_returned, written) = batch.commit_file(false).unwrap();
        assert_eq!(written as usize, payload.len());

        let content = fs::read(&path).unwrap();
        assert_eq!(content, payload);
    }

    #[test]
    fn multi_file_sequential_writes() {
        let dir = tempdir().unwrap();
        let mut batch = IocpDiskBatch::new(&IocpConfig::default()).unwrap();

        let test_data: Vec<(&str, Vec<u8>)> = vec![
            ("file_a.bin", vec![0xAA; 1024]),
            ("file_b.bin", vec![0xBB; 4096]),
            ("file_c.bin", vec![0xCC; 128]),
        ];

        for (name, data) in &test_data {
            let path = dir.path().join(name);
            let file = open_writable(&path);
            batch.begin_file(file).unwrap();
            batch.write_data(data).unwrap();
            let (_returned, written) = batch.commit_file(false).unwrap();
            assert_eq!(written as usize, data.len());
        }

        for (name, data) in &test_data {
            let content = fs::read(dir.path().join(name)).unwrap();
            assert_eq!(content, *data, "content mismatch for {name}");
        }
    }

    #[test]
    fn large_write_exceeds_buffer_drains_via_completion_port() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("large.bin");
        let file = open_writable(&path);

        let config = IocpConfig {
            buffer_size: 4096,
            concurrent_ops: 4,
            ..IocpConfig::default()
        };
        let mut batch = IocpDiskBatch::new(&config).unwrap();
        batch.begin_file(file).unwrap();

        let data: Vec<u8> = (0..32_768).map(|i| (i % 256) as u8).collect();
        batch.write_data(&data).unwrap();
        let (_returned, written) = batch.commit_file(false).unwrap();
        assert_eq!(written as usize, data.len());

        let content = fs::read(&path).unwrap();
        assert_eq!(content, data);
    }

    #[test]
    fn commit_with_fsync_calls_flush_file_buffers() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("fsync.bin");
        let file = open_writable(&path);

        let mut batch = IocpDiskBatch::new(&IocpConfig::default()).unwrap();
        batch.begin_file(file).unwrap();
        batch.write_data(b"durable").unwrap();
        let (_returned, written) = batch.commit_file(true).unwrap();
        assert_eq!(written, 7);

        let content = fs::read(&path).unwrap();
        assert_eq!(content, b"durable");
    }

    #[test]
    fn begin_file_flushes_previous() {
        let dir = tempdir().unwrap();
        let mut batch = IocpDiskBatch::new(&IocpConfig::default()).unwrap();

        let path1 = dir.path().join("first.bin");
        let file1 = open_writable(&path1);
        batch.begin_file(file1).unwrap();
        batch.write_data(b"first file data").unwrap();

        let path2 = dir.path().join("second.bin");
        let file2 = open_writable(&path2);
        batch.begin_file(file2).unwrap();

        // First file should be on disk after the rotation flush.
        let content1 = fs::read(&path1).unwrap();
        assert_eq!(content1, b"first file data");

        batch.write_data(b"second").unwrap();
        let (_returned, written) = batch.commit_file(false).unwrap();
        assert_eq!(written, 6);
    }

    #[test]
    fn drop_flushes_pending_data() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("drop_flush.bin");

        {
            let mut batch = IocpDiskBatch::new(&IocpConfig::default()).unwrap();
            let file = open_writable(&path);
            batch.begin_file(file).unwrap();
            batch.write_data(b"drop test").unwrap();
            // No explicit commit - rely on Drop.
        }

        let content = fs::read(&path).unwrap();
        assert_eq!(content, b"drop test");
    }

    #[test]
    fn write_trait_implementation_round_trips() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("write_trait.bin");
        let file = open_writable(&path);

        let mut batch = IocpDiskBatch::new(&IocpConfig::default()).unwrap();
        batch.begin_file(file).unwrap();

        let n = Write::write(&mut batch, b"hello ").unwrap();
        assert_eq!(n, 6);
        Write::write_all(&mut batch, b"world").unwrap();
        Write::flush(&mut batch).unwrap();

        let (_returned, written) = batch.commit_file(false).unwrap();
        assert_eq!(written, 11);

        let content = fs::read(&path).unwrap();
        assert_eq!(content, b"hello world");
    }

    #[test]
    fn batched_submission_submits_n_chunks() {
        // Pick buffer_size and concurrent_ops so the data triggers multiple
        // overlapped submissions per flush.
        let dir = tempdir().unwrap();
        let path = dir.path().join("batched.bin");
        let file = open_writable(&path);

        let config = IocpConfig {
            buffer_size: 1024,
            concurrent_ops: 8,
            ..IocpConfig::default()
        };
        let mut batch = IocpDiskBatch::new(&config).unwrap();
        batch.begin_file(file).unwrap();

        // 16 chunks of 1 KB each = 16 KB total. With 8 in-flight, two drain
        // cycles are needed.
        let data: Vec<u8> = (0..16 * 1024).map(|i| (i & 0xFF) as u8).collect();
        batch.write_data(&data).unwrap();
        let (_returned, written) = batch.commit_file(false).unwrap();
        assert_eq!(written as usize, data.len());

        let content = fs::read(&path).unwrap();
        assert_eq!(content, data);
    }

    #[test]
    fn error_propagates_when_reopen_overlapped_fails() {
        // Open with read-only access. begin_file calls ReOpenFile asking
        // for FILE_GENERIC_WRITE which the original handle was not opened
        // with, causing ReOpenFile to fail.
        use windows_sys::Win32::Storage::FileSystem::{FILE_GENERIC_READ, OPEN_EXISTING};

        let config = IocpConfig::default();
        let mut batch = IocpDiskBatch::new(&config).unwrap();

        let dir = tempdir().unwrap();
        let path = dir.path().join("readonly_target.bin");
        std::fs::write(&path, b"existing").unwrap();

        let wide = to_wide(&path);
        // SAFETY: standard Win32 open with read-only access.
        #[allow(unsafe_code)]
        let handle = unsafe {
            CreateFileW(
                wide.as_ptr(),
                FILE_GENERIC_READ,
                FILE_SHARE_READ,
                std::ptr::null(),
                OPEN_EXISTING,
                FILE_ATTRIBUTE_NORMAL,
                std::ptr::null_mut(),
            )
        };
        assert_ne!(handle, INVALID_HANDLE_VALUE);
        // SAFETY: `handle` is freshly opened and exclusively owned here.
        #[allow(unsafe_code)]
        let file = unsafe {
            use std::os::windows::io::FromRawHandle;
            File::from_raw_handle(handle as *mut std::ffi::c_void)
        };

        let result = batch.begin_file(file);
        assert!(
            result.is_err(),
            "begin_file must surface ReOpenFile failure when the original handle lacks write access"
        );
    }

    #[test]
    fn no_leaked_overlapped_handles_after_many_rotations() {
        // Round-trip many begin/commit cycles. If the overlapped handle were
        // leaked per file the process would eventually exhaust its handle
        // table; here we exercise the path 32 times and verify each file
        // lands intact.
        let dir = tempdir().unwrap();
        let mut batch = IocpDiskBatch::new(&IocpConfig::default()).unwrap();

        for i in 0..32 {
            let path = dir.path().join(format!("rotated_{i}.bin"));
            let file = open_writable(&path);
            batch.begin_file(file).unwrap();
            let payload = format!("rotation #{i}");
            batch.write_data(payload.as_bytes()).unwrap();
            let (_returned, written) = batch.commit_file(false).unwrap();
            assert_eq!(written as usize, payload.len());

            let content = fs::read(&path).unwrap();
            assert_eq!(content, payload.as_bytes());
        }
    }

    #[test]
    fn completion_ordering_independent_of_submission_order() {
        // Multiple in-flight writes may complete out of order. The drain
        // loop must reconcile each completion with its OVERLAPPED pointer
        // and produce the correct file contents regardless of order.
        let dir = tempdir().unwrap();
        let path = dir.path().join("ordering.bin");
        let file = open_writable(&path);

        let config = IocpConfig {
            buffer_size: 4096,
            concurrent_ops: 8,
            ..IocpConfig::default()
        };
        let mut batch = IocpDiskBatch::new(&config).unwrap();
        batch.begin_file(file).unwrap();

        // 8 distinct chunks of 4 KB each, each tagged with its index so we
        // can verify positional correctness regardless of completion order.
        let mut data = Vec::with_capacity(8 * 4096);
        for chunk_idx in 0..8u8 {
            data.extend(std::iter::repeat(chunk_idx).take(4096));
        }
        batch.write_data(&data).unwrap();
        let (_returned, written) = batch.commit_file(false).unwrap();
        assert_eq!(written as usize, data.len());

        let content = fs::read(&path).unwrap();
        assert_eq!(content, data);
    }
}
