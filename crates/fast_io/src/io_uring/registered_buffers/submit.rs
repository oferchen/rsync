//! `ReadFixed` / `WriteFixed` batch submission helpers for registered buffers.
//!
//! These helpers convert a slice of registered-buffer slot pointers into a
//! batched submission against an io_uring instance, handling short reads,
//! out-of-order completions, and per-chunk offset arithmetic. The
//! [`RegisteredBufferSlotInfo`] DTO decouples callers from the lifetime of
//! [`super::RegisteredBufferSlot`] so the batch helpers can take borrowed
//! slot metadata without a generic lifetime parameter.

use std::io;
use std::ptr;

use io_uring::IoUring as RawIoUring;

/// Submits a batch of `ReadFixed` SQEs reading into registered buffers.
///
/// Reads `total_len` bytes from the file starting at `base_offset`, using
/// registered buffers from `slots`. Each slot handles one chunk of data.
/// Completions are collected and the total bytes read is returned.
///
/// The `slots` parameter provides buffer indices and pointers. Callers must
/// ensure slots are checked out from a `RegisteredBufferGroup` that is
/// registered with the same ring.
///
/// Exposed publicly under `#[doc(hidden)]` so integration tests in
/// `crates/fast_io/tests/` can exercise short-read paths (NFS, FUSE,
/// slow block devices). Not part of the stable public API.
#[doc(hidden)]
pub fn submit_read_fixed_batch(
    ring: &mut RawIoUring,
    fd: io_uring::types::Fd,
    output: &mut [u8],
    base_offset: u64,
    slots: &[RegisteredBufferSlotInfo],
    fixed_fd_slot: i32,
) -> io::Result<usize> {
    use super::super::batching::maybe_fixed_file;
    use io_uring::opcode::ReadFixed;

    if output.is_empty() || slots.is_empty() {
        return Ok(0);
    }

    let mut total_read = 0usize;
    let total = output.len();
    let chunk_size = slots[0].buffer_size;

    // Process in rounds, one SQE per slot per round.
    while total_read < total {
        let remaining = total - total_read;
        let n_sqes = remaining.div_ceil(chunk_size).min(slots.len());
        let mut submitted = 0u32;

        // Track how many bytes each SQE requested for short-read detection.
        let mut requested_per_sqe: Vec<usize> = Vec::with_capacity(n_sqes);

        for (i, slot) in slots.iter().enumerate().take(n_sqes) {
            let offset_in_output = total_read + i * chunk_size;
            let want = chunk_size.min(total - offset_in_output);
            let file_offset = base_offset + offset_in_output as u64;

            let entry = ReadFixed::new(fd, slot.ptr, want as u32, slot.buf_index)
                .offset(file_offset)
                .build()
                .user_data(i as u64);
            let entry = maybe_fixed_file(entry, fixed_fd_slot);

            // Safety: the registered buffer at slot is valid and pinned for
            // the duration of this submit_and_wait cycle.
            unsafe {
                ring.submission()
                    .push(&entry)
                    .map_err(|_| io::Error::other("submission queue full"))?;
            }
            requested_per_sqe.push(want);
            submitted += 1;
        }

        if submitted == 0 {
            break;
        }

        ring.submit_and_wait(submitted as usize)?;

        // Collect actual bytes read per SQE index. CQEs may arrive out of order.
        let mut actual_per_sqe = vec![0usize; submitted as usize];

        let mut completed = 0u32;
        while completed < submitted {
            let cqe = ring
                .completion()
                .next()
                .ok_or_else(|| io::Error::other("missing CQE"))?;

            let idx = cqe.user_data() as usize;
            let result = cqe.result();

            if result < 0 {
                return Err(io::Error::from_raw_os_error(-result));
            }

            let bytes = result as usize;
            actual_per_sqe[idx] = bytes;

            let out_start = total_read + idx * chunk_size;
            let out_end = (out_start + bytes).min(total);
            let copy_len = out_end - out_start;

            // Safety: the kernel wrote `bytes` into the registered buffer.
            // We copy from the registered buffer into the caller's output slice.
            unsafe {
                ptr::copy_nonoverlapping(
                    slots[idx].ptr,
                    output[out_start..].as_mut_ptr(),
                    copy_len,
                );
            }

            completed += 1;
        }

        // Advance by the contiguous prefix of fully-read SQEs. If SQE `i`
        // returned fewer bytes than requested (short read - common on NFS,
        // FUSE, and slow block devices), we stop at that point so the outer
        // loop retries from the correct offset.
        let mut batch_advance = 0usize;
        for i in 0..submitted as usize {
            batch_advance += actual_per_sqe[i];
            if actual_per_sqe[i] < requested_per_sqe[i] {
                break;
            }
        }

        if batch_advance == 0 {
            break; // EOF or zero-length read - avoid infinite loop.
        }
        total_read += batch_advance;
    }

    // submit_read_fixed_batch invariant: on the success path the reported
    // byte count must never exceed the caller's output capacity. A regression
    // here would let callers read uninitialised tail memory or write past
    // their slice. The per-CQE copy is already clamped via `out_end.min(total)`
    // so this guards the returned length itself.
    let reported = total_read.min(total);
    debug_assert!(
        reported <= output.len(),
        "submit_read_fixed_batch returned {reported} bytes for an output of {} bytes",
        output.len()
    );
    Ok(reported)
}

/// Submits a batch of `WriteFixed` SQEs writing from registered buffers.
///
/// Writes `data` to the file starting at `base_offset`, copying chunks into
/// registered buffers and submitting `WriteFixed` SQEs. Returns the total
/// bytes written.
///
/// Gated to `#[cfg(test)]`: the per-thread-ring migration removed the
/// production caller; the function is preserved for the existing batch
/// tests until IUR-3.e reintroduces a bgid-lease-aware replacement.
#[cfg(test)]
pub(in crate::io_uring) fn submit_write_fixed_batch(
    ring: &mut RawIoUring,
    fd: io_uring::types::Fd,
    data: &[u8],
    base_offset: u64,
    slots: &[RegisteredBufferSlotInfo],
    fixed_fd_slot: i32,
) -> io::Result<usize> {
    use super::super::batching::maybe_fixed_file;
    use io_uring::opcode::WriteFixed;

    if data.is_empty() || slots.is_empty() {
        return Ok(0);
    }

    let total = data.len();
    let mut total_written = 0usize;
    let chunk_size = slots[0].buffer_size;

    while total_written < total {
        let remaining = total - total_written;
        let n_sqes = remaining.div_ceil(chunk_size).min(slots.len());
        let mut submitted = 0u32;

        for (i, slot) in slots.iter().enumerate().take(n_sqes) {
            let src_start = total_written + i * chunk_size;
            let want = chunk_size.min(total - src_start);
            let file_offset = base_offset + src_start as u64;

            // Copy data into registered buffer.
            // Safety: registered buffer at slot is valid and large enough.
            unsafe {
                ptr::copy_nonoverlapping(data[src_start..].as_ptr(), slot.ptr, want);
            }

            let entry = WriteFixed::new(fd, slot.ptr, want as u32, slot.buf_index)
                .offset(file_offset)
                .build()
                .user_data(i as u64);
            let entry = maybe_fixed_file(entry, fixed_fd_slot);

            // Safety: the registered buffer contains valid data and is pinned
            // for the duration of this submit_and_wait cycle.
            unsafe {
                ring.submission()
                    .push(&entry)
                    .map_err(|_| io::Error::other("submission queue full"))?;
            }
            submitted += 1;
        }

        if submitted == 0 {
            break;
        }

        ring.submit_and_wait(submitted as usize)?;

        let mut batch_written = 0usize;
        let mut completed = 0u32;
        while completed < submitted {
            let cqe = ring
                .completion()
                .next()
                .ok_or_else(|| io::Error::other("missing CQE"))?;

            let result = cqe.result();
            if result < 0 {
                return Err(io::Error::from_raw_os_error(-result));
            }
            if result == 0 {
                return Err(io::Error::new(
                    io::ErrorKind::WriteZero,
                    "write_fixed returned 0 bytes",
                ));
            }

            batch_written += result as usize;
            completed += 1;
        }

        total_written += batch_written;
    }

    Ok(total_written)
}

/// Lightweight info struct for passing registered buffer metadata to batch helpers.
///
/// Avoids lifetime complications of passing `RegisteredBufferSlot` references
/// into the batch submission functions.
///
/// Exposed publicly under `#[doc(hidden)]` to allow integration tests to
/// drive [`submit_read_fixed_batch`]. Not part of the stable public API.
#[doc(hidden)]
pub struct RegisteredBufferSlotInfo {
    /// Raw pointer to the registered buffer memory.
    pub ptr: *mut u8,
    /// Buffer index for `ReadFixed`/`WriteFixed` SQEs.
    pub buf_index: u16,
    /// Size of the buffer in bytes.
    pub buffer_size: usize,
}
