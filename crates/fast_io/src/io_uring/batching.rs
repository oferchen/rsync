//! Batched SQE submission helpers for io_uring file and socket I/O.

use std::io;

use io_uring::{IoUring as RawIoUring, opcode, types};

/// Sentinel for "no fixed fd"; use raw fd path.
pub(super) const NO_FIXED_FD: i32 = -1;

/// Returns the fd `types::Fd` for an SQE, using the fixed-file slot when
/// registered, or the raw fd otherwise.
pub(super) fn sqe_fd(raw_fd: i32, fixed_fd_slot: i32) -> types::Fd {
    if fixed_fd_slot != NO_FIXED_FD {
        types::Fd(fixed_fd_slot)
    } else {
        types::Fd(raw_fd)
    }
}

/// Sets the `IOSQE_FIXED_FILE` flag on an SQE when using registered files.
pub(super) fn maybe_fixed_file(
    entry: io_uring::squeue::Entry,
    fixed_fd_slot: i32,
) -> io_uring::squeue::Entry {
    if fixed_fd_slot != NO_FIXED_FD {
        entry.flags(io_uring::squeue::Flags::FIXED_FILE)
    } else {
        entry
    }
}

/// Registers `raw_fd` with `ring` if `register` is true. Returns the
/// fixed-file slot (0) on success, or `NO_FIXED_FD` on failure / opt-out.
pub(super) fn try_register_fd(ring: &RawIoUring, raw_fd: i32, register: bool) -> i32 {
    if register {
        let fds = [raw_fd];
        match ring.submitter().register_files(&fds) {
            Ok(()) => 0,
            Err(_) => NO_FIXED_FD,
        }
    } else {
        NO_FIXED_FD
    }
}

/// Submits a batch of write SQEs from contiguous `data` and collects completions.
///
/// Splits `data` into `chunk_size`-sized pieces, submitting up to `max_sqes` at
/// a time. Handles short writes by resubmitting the remainder.
///
/// When `fixed_fd_slot` is not `NO_FIXED_FD`, SQEs use the registered fixed-file
/// index and set `IOSQE_FIXED_FILE`.
pub(super) fn submit_write_batch(
    ring: &mut RawIoUring,
    fd: types::Fd,
    data: &[u8],
    base_offset: u64,
    chunk_size: usize,
    max_sqes: usize,
    fixed_fd_slot: i32,
) -> io::Result<usize> {
    if data.is_empty() {
        return Ok(0);
    }

    let total = data.len();
    let mut global_done = 0usize;

    while global_done < total {
        let remaining = total - global_done;

        // Build a batch of chunks from the remaining data.
        let n_chunks = remaining.div_ceil(chunk_size).min(max_sqes);
        // Per-chunk tracking: (chunk_start_in_data, chunk_len, bytes_written_so_far).
        let mut slots: Vec<(usize, usize, usize)> = Vec::with_capacity(n_chunks);
        for i in 0..n_chunks {
            let start = global_done + i * chunk_size;
            let len = chunk_size.min(total - start);
            slots.push((start, len, 0));
        }

        let mut batch_complete = false;
        while !batch_complete {
            let mut submitted = 0u32;
            for (idx, &(start, len, done)) in slots.iter().enumerate() {
                let want = len - done;
                if want == 0 {
                    continue;
                }
                let file_off = base_offset + (start + done) as u64;
                let entry = opcode::Write::new(fd, data[start + done..].as_ptr(), want as u32)
                    .offset(file_off)
                    .build()
                    .user_data(idx as u64);
                let entry = maybe_fixed_file(entry, fixed_fd_slot);

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
                if result == 0 {
                    return Err(io::Error::new(
                        io::ErrorKind::WriteZero,
                        "write returned 0 bytes",
                    ));
                }

                slots[idx].2 += result as usize;
                completed += 1;
            }

            batch_complete = slots.iter().all(|&(_, len, done)| done >= len);
        }

        let batch_written: usize = slots.iter().map(|&(_, _, done)| done).sum();
        global_done += batch_written;
    }

    Ok(global_done)
}

/// Submits a batch of send SQEs from contiguous `data` and collects completions.
///
/// Analogous to [`submit_write_batch`] but uses `opcode::Send` instead of
/// `opcode::Write` and omits file offsets (stream sockets have no position).
pub(super) fn submit_send_batch(
    ring: &mut RawIoUring,
    fd: types::Fd,
    data: &[u8],
    chunk_size: usize,
    max_sqes: usize,
    fixed_fd_slot: i32,
) -> io::Result<usize> {
    if data.is_empty() {
        return Ok(0);
    }

    let total = data.len();
    let mut global_done = 0usize;

    while global_done < total {
        let remaining = total - global_done;
        let n_chunks = remaining.div_ceil(chunk_size).min(max_sqes);
        let mut slots: Vec<(usize, usize, usize)> = Vec::with_capacity(n_chunks);
        for i in 0..n_chunks {
            let start = global_done + i * chunk_size;
            let len = chunk_size.min(total - start);
            slots.push((start, len, 0));
        }

        let mut batch_complete = false;
        while !batch_complete {
            let mut submitted = 0u32;
            for (idx, &(start, len, done)) in slots.iter().enumerate() {
                let want = len - done;
                if want == 0 {
                    continue;
                }
                let entry = opcode::Send::new(
                    sqe_fd(fd.0, fixed_fd_slot),
                    data[start + done..].as_ptr(),
                    want as u32,
                )
                .build()
                .user_data(idx as u64);
                let entry = maybe_fixed_file(entry, fixed_fd_slot);

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
                if result == 0 {
                    return Err(io::Error::new(
                        io::ErrorKind::WriteZero,
                        "send returned 0 bytes",
                    ));
                }

                slots[idx].2 += result as usize;
                completed += 1;
            }

            batch_complete = slots.iter().all(|&(_, len, done)| done >= len);
        }

        let batch_sent: usize = slots.iter().map(|&(_, _, done)| done).sum();
        global_done += batch_sent;
    }

    Ok(global_done)
}
