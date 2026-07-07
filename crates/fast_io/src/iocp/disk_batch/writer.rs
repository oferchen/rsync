//! `WriteFile` dispatch and overlapped-handle lifecycle for [`super::IocpDiskBatch`].
//!
//! Owns the submission half of the batched IOCP pipeline: reopening caller
//! file handles with `FILE_FLAG_OVERLAPPED` (plus optional
//! `FILE_FLAG_NO_BUFFERING` / `FILE_FLAG_WRITE_THROUGH`), issuing chunked
//! overlapped `WriteFile` calls, and shepherding completions back through
//! the drain loop in [`super::completion`].

use std::io;
use std::pin::Pin;
use std::sync::atomic::Ordering;

use windows_sys::Win32::Foundation::{CloseHandle, HANDLE, INVALID_HANDLE_VALUE, TRUE};
use windows_sys::Win32::Storage::FileSystem::{
    FILE_FLAG_NO_BUFFERING, FILE_FLAG_OVERLAPPED, FILE_FLAG_WRITE_THROUGH, FILE_GENERIC_WRITE,
    FILE_SHARE_DELETE, FILE_SHARE_READ, FILE_SHARE_WRITE, ReOpenFile, WriteFile,
};
use windows_sys::Win32::System::IO::OVERLAPPED;

use crate::iocp::completion_port::CompletionPort;
use crate::iocp::config::IocpConfig;
use crate::iocp::overlapped::OverlappedOp;

use super::buffer::BOUNCE_COPIES_AVOIDED;
use super::completion::{
    drain_all_ignoring_completion_errors, drain_completions, take_injected_write_error,
};

/// RAII guard owning the overlapped handle returned by [`reopen_overlapped`].
///
/// The handle is closed exactly once on drop, so every early-return and error
/// path releases it without a manual `CloseHandle` at each call site. Callers
/// that must hand the raw handle to a Win32 API borrow it via [`Self::raw`];
/// the guard retains ownership and the borrowed handle must not be closed by
/// the caller. The wrapped handle is never null or `INVALID_HANDLE_VALUE`
/// because [`reopen_overlapped`] rejects those before constructing the guard.
pub(super) struct OverlappedHandle {
    handle: HANDLE,
}

impl OverlappedHandle {
    /// Borrows the raw handle for use with Win32 APIs. Ownership stays with
    /// the guard; the returned handle must not be closed by the caller.
    pub(super) fn raw(&self) -> HANDLE {
        self.handle
    }
}

impl Drop for OverlappedHandle {
    fn drop(&mut self) {
        close_overlapped_handle(self.handle);
    }
}

/// Reopens an existing file handle with `FILE_FLAG_OVERLAPPED` so it can be
/// associated with a completion port.
///
/// Mirrors the Microsoft-documented pattern for converting an
/// already-opened handle into one that supports overlapped I/O without
/// reopening the path. When `config.unbuffered` is set the reopened handle
/// also receives `FILE_FLAG_NO_BUFFERING` so submissions skip the system
/// cache; combined with the page-aligned buffer chunks the writer issues,
/// the kernel can dispatch each `WriteFile` without a bounce copy.
/// `config.write_through` similarly maps to `FILE_FLAG_WRITE_THROUGH`.
/// The returned [`OverlappedHandle`] closes the underlying handle on drop.
pub(super) fn reopen_overlapped(
    handle: HANDLE,
    config: &IocpConfig,
) -> io::Result<OverlappedHandle> {
    let mut flags = FILE_FLAG_OVERLAPPED;
    if config.unbuffered {
        flags |= FILE_FLAG_NO_BUFFERING;
    }
    if config.write_through {
        flags |= FILE_FLAG_WRITE_THROUGH;
    }

    // SAFETY: `handle` is borrowed from the caller's live File for the
    // duration of the call. ReOpenFile returns a new handle with the
    // requested access/share/flag combination; failure is signalled by
    // INVALID_HANDLE_VALUE per Microsoft docs.
    #[allow(unsafe_code)]
    let new_handle = unsafe {
        ReOpenFile(
            handle,
            FILE_GENERIC_WRITE,
            FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
            flags,
        )
    };

    if new_handle == INVALID_HANDLE_VALUE {
        return Err(io::Error::last_os_error());
    }

    Ok(OverlappedHandle { handle: new_handle })
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
///
/// `bytes_written_out` is updated with the count of bytes that reached the
/// kernel before the function returns. On error the count reflects every
/// completion drained successfully prior to the failure so the caller can
/// advance its file-offset bookkeeping past the durable prefix instead of
/// retrying writes that already landed.
pub(super) fn submit_write_batch(
    port: &CompletionPort,
    handle: HANDLE,
    data: &[u8],
    base_offset: u64,
    chunk_size: usize,
    max_in_flight: usize,
    use_aligned: bool,
    bytes_written_out: &mut usize,
) -> io::Result<()> {
    *bytes_written_out = 0;
    if data.is_empty() {
        return Ok(());
    }

    let total = data.len();
    let mut next_chunk_start = 0usize;
    let mut in_flight: Vec<Pin<Box<OverlappedOp>>> = Vec::with_capacity(max_in_flight);

    while next_chunk_start < total || !in_flight.is_empty() {
        // Fill the in-flight queue up to the configured limit.
        while in_flight.len() < max_in_flight && next_chunk_start < total {
            let len = chunk_size.min(total - next_chunk_start);
            let chunk = &data[next_chunk_start..next_chunk_start + len];
            let offset = base_offset + next_chunk_start as u64;
            let op = match submit_one_write(handle, offset, chunk, use_aligned) {
                Ok(op) => op,
                Err(e) => {
                    // This submission failed synchronously and never queued,
                    // but earlier iterations pushed ops the kernel has already
                    // accepted (ERROR_IO_PENDING). Those ops own pinned
                    // buffers the kernel may still be writing into. Dropping
                    // `in_flight` now would free them under the kernel - a
                    // use-after-free with late completions dereferencing freed
                    // OVERLAPPED structs. Reap every outstanding completion
                    // first (crediting the durable prefix), then surface the
                    // original submission error.
                    *bytes_written_out +=
                        drain_all_ignoring_completion_errors(port, in_flight.len());
                    return Err(e);
                }
            };
            in_flight.push(op);
            next_chunk_start += len;
        }

        if in_flight.is_empty() {
            break;
        }

        // Reap at least one completion. The drain retires every OVERLAPPED the
        // kernel dequeued this pass (successful or faulted) and reports the
        // first fault, if any, without abandoning the sibling ops. A drain-side
        // syscall failure still propagates, but only after we drain the ops
        // this call could not reap - dropping their pinned buffers while the
        // kernel still owns them would be a use-after-free.
        let outcome = match drain_completions(port, in_flight.len()) {
            Ok(outcome) => outcome,
            Err(e) => {
                *bytes_written_out += drain_all_ignoring_completion_errors(port, in_flight.len());
                return Err(e);
            }
        };
        let mut resubmissions: Vec<(u64, Vec<u8>)> = Vec::new();
        let mut zero_byte_completion = false;

        in_flight.retain_mut(|op| {
            let ptr = pinned_overlapped_addr(op);
            if let Some(transferred) = outcome
                .completions
                .iter()
                .find_map(|(p, n)| if *p == ptr { Some(*n) } else { None })
            {
                let chunk_len = op.buffer.len();
                if transferred == chunk_len {
                    *bytes_written_out += transferred;
                    false
                } else if transferred == 0 {
                    zero_byte_completion = true;
                    false
                } else {
                    // Short write: resubmit the unwritten tail at the
                    // appropriate offset.
                    *bytes_written_out += transferred;
                    let remaining = op.buffer.as_slice()[transferred..].to_vec();
                    let original_offset = read_offset(&op.overlapped);
                    let new_offset = original_offset + transferred as u64;
                    resubmissions.push((new_offset, remaining));
                    false
                }
            } else {
                // Retire faulted ops too: the kernel dequeued them, so their
                // pinned buffers are done. Keeping them in-flight would either
                // hang the residual drain or free the box while the kernel
                // still owned it. Retained ops are the ones still outstanding.
                !outcome.retired.contains(&ptr)
            }
        });

        // A completion reported a faulted NTSTATUS: retire everything this pass
        // dequeued (done above), then drain the residual outstanding ops before
        // dropping their pinned buffers, and surface the original error.
        if let Some(err) = outcome.error {
            *bytes_written_out += drain_all_ignoring_completion_errors(port, in_flight.len());
            return Err(err);
        }

        if zero_byte_completion {
            // Reap the ops still outstanding after this partial drain before
            // dropping their pinned buffers under the kernel.
            *bytes_written_out += drain_all_ignoring_completion_errors(port, in_flight.len());
            return Err(io::Error::new(
                io::ErrorKind::WriteZero,
                "overlapped write returned zero bytes",
            ));
        }

        for (offset, remaining) in resubmissions {
            let op = match submit_one_write(handle, offset, &remaining, use_aligned) {
                Ok(op) => op,
                Err(e) => {
                    // A resubmission failed synchronously while earlier ops
                    // remain in flight; drain them before dropping the boxes.
                    *bytes_written_out +=
                        drain_all_ignoring_completion_errors(port, in_flight.len());
                    return Err(e);
                }
            };
            in_flight.push(op);
        }
    }

    Ok(())
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
    use_aligned: bool,
) -> io::Result<Pin<Box<OverlappedOp>>> {
    let mut op = if use_aligned {
        OverlappedOp::new_write_aligned(offset, data)
    } else {
        OverlappedOp::new_write(offset, data)
    };
    let overlapped_ptr = op.as_overlapped_ptr();

    // Test-only fault injection hook. Returns a synthetic Win32 error code
    // before any kernel call so the drain loop and caller error mapping can
    // be exercised deterministically (e.g. ERROR_DISK_FULL coverage in
    // `crates/fast_io/tests/iocp_disk_full_simulation.rs`). Dormant in
    // production - the inner check is a single relaxed atomic load.
    if let Some(code) = take_injected_write_error() {
        return Err(io::Error::from_raw_os_error(code));
    }

    let mut bytes_written: u32 = 0;

    // SAFETY: `handle` is a valid overlapped file handle owned by the
    // active-file slot. The op buffer and OVERLAPPED are pinned for the
    // duration of the asynchronous call. When `use_aligned` is true the
    // buffer pointer is page-aligned, so the kernel can dispatch the I/O
    // on a `FILE_FLAG_NO_BUFFERING` handle without an aligned-scratch
    // bounce copy - bump the telemetry counter to reflect the saving.
    if use_aligned {
        BOUNCE_COPIES_AVOIDED.fetch_add(1, Ordering::Relaxed);
    }
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
