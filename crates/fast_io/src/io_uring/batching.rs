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

/// Sentinel user_data tag for a `PollAdd(POLLOUT)` gating CQE.
///
/// Picked far above any plausible SEND/WRITE batch index so it cannot collide
/// with a slot index in [`submit_send_batch`].
const POLL_OUT_USER_DATA: u64 = u64::MAX;

/// Sentinel user_data tag for the linked-timeout CQE that cancels a pending
/// `PollAdd(POLLOUT)`.
const POLL_OUT_TIMEOUT_USER_DATA: u64 = u64::MAX - 1;

/// Waits for the socket to become writable using `IORING_OP_POLL_ADD` with
/// `POLLOUT`.
///
/// Without a readiness gate, an `IORING_OP_SEND` SQE on a back-pressured TCP
/// socket can sit in the kernel until the send buffer drains. While that SQE
/// is pending the ring's `submit_and_wait` does not return, which means any
/// concurrent `IORING_OP_RECV` completion on the same ring stays unreaped --
/// the symptom in issue #1872. By polling for `POLLOUT` first we let the
/// receive side keep draining; only when the kernel has room do we submit
/// the SEND.
///
/// A linked `Timeout` SQE bounds the wait so callers cannot block forever
/// (e.g., the peer froze entirely). On expiry we surface `WouldBlock` so the
/// caller can re-arm without treating it as a fatal error.
///
/// upstream: io.c:perform_io -- `FD_SET(out_fd, &w_fds); select(... &w_fds ...)`
fn poll_writable(
    ring: &mut RawIoUring,
    fd: types::Fd,
    fixed_fd_slot: i32,
    timeout: &types::Timespec,
) -> io::Result<()> {
    // POLLOUT is a stable kernel UAPI constant (4); the libc::POLLOUT alias
    // is `c_short`, so widen it to the `u32` mask io_uring expects.
    let pollout_mask: u32 = libc::POLLOUT as u32;

    // Build the PollAdd with IO_LINK; if the fd is registered, OR in
    // FIXED_FILE rather than letting `maybe_fixed_file` overwrite IO_LINK.
    let mut sqe_flags = io_uring::squeue::Flags::IO_LINK;
    if fixed_fd_slot != NO_FIXED_FD {
        sqe_flags |= io_uring::squeue::Flags::FIXED_FILE;
    }
    let poll_entry = opcode::PollAdd::new(sqe_fd(fd.0, fixed_fd_slot), pollout_mask)
        .build()
        .flags(sqe_flags)
        .user_data(POLL_OUT_USER_DATA);

    let timeout_entry = opcode::LinkTimeout::new(timeout as *const types::Timespec)
        .build()
        .user_data(POLL_OUT_TIMEOUT_USER_DATA);

    // SAFETY: Both SQEs reference data that outlives this submission: the fd
    // is owned by the caller's socket, and `timeout` is borrowed for the
    // duration of the call. The poll SQE carries `IO_LINK` so the kernel
    // atomically applies the immediately following `LinkTimeout`. We drain
    // every completion synchronously below before returning, so neither SQE
    // payload outlives this stack frame.
    unsafe {
        let mut sq = ring.submission();
        sq.push(&poll_entry)
            .map_err(|_| io::Error::other("submission queue full"))?;
        sq.push(&timeout_entry)
            .map_err(|_| io::Error::other("submission queue full"))?;
    }

    // Wait for at least one completion. The poll and the linked timeout both
    // post CQEs; whichever fires first will satisfy this wait. We then drain
    // the second CQE (it is already queued) without re-entering the kernel.
    ring.submit_and_wait(1)?;

    let mut poll_result: Option<i32> = None;
    let mut saw_timeout = false;
    loop {
        let cqe = match ring.completion().next() {
            Some(c) => c,
            None => break,
        };
        match cqe.user_data() {
            POLL_OUT_USER_DATA => poll_result = Some(cqe.result()),
            POLL_OUT_TIMEOUT_USER_DATA => saw_timeout = true,
            _ => {}
        }
    }

    match poll_result {
        Some(r) if r >= 0 => Ok(()),
        Some(neg) => {
            let err = io::Error::from_raw_os_error(-neg);
            // `ECANCELED` is what the poll reports when the linked timeout
            // fired first; treat that and an explicit `ETIME` as transient
            // backpressure.
            if matches!(
                err.raw_os_error(),
                Some(libc::ETIME) | Some(libc::ECANCELED)
            ) {
                Err(io::Error::new(
                    io::ErrorKind::WouldBlock,
                    "POLLOUT timed out waiting for socket writability",
                ))
            } else {
                Err(err)
            }
        }
        None if saw_timeout => Err(io::Error::new(
            io::ErrorKind::WouldBlock,
            "POLLOUT linked timeout fired before poll completion",
        )),
        None => Err(io::Error::other("missing POLLOUT CQE")),
    }
}

/// Submits a batch of send SQEs from contiguous `data` and collects completions.
///
/// Analogous to [`submit_write_batch`] but uses `opcode::Send` instead of
/// `opcode::Write` and omits file offsets (stream sockets have no position).
///
/// To prevent the receive side from starving when the kernel send buffer is
/// full (issue #1872), each batch is gated by a `IORING_OP_POLL_ADD(POLLOUT)`
/// SQE. The SEND SQEs are only submitted once the socket reports writable,
/// mirroring the bidirectional `select()` strategy in upstream rsync's
/// `perform_io()`. This keeps `submit_and_wait` from being held hostage by a
/// back-pressured SEND that would otherwise leave concurrent RECV CQEs
/// un-reaped.
///
/// upstream: io.c:perform_io
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

    // Per-iteration readiness ceiling. Re-armed each loop pass; a back-
    // pressured but progressing peer never trips it. 30 s matches the
    // default upstream `select_timeout`.
    let timeout = types::Timespec::new().sec(30).nsec(0);
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
            // Gate on POLLOUT. Transient backpressure surfaces as WouldBlock;
            // we re-arm and continue without escalating to the caller.
            match poll_writable(ring, fd, fixed_fd_slot, &timeout) {
                Ok(()) => {}
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => continue,
                Err(e) => return Err(e),
            }

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
                    let err = io::Error::from_raw_os_error(-result);
                    // EAGAIN/EWOULDBLOCK on a non-blocking-style send: rearm
                    // the readiness poll on the next outer loop iteration
                    // rather than failing. EWOULDBLOCK == EAGAIN on Linux per
                    // POSIX, so a single arm covers both names.
                    if err.raw_os_error() == Some(libc::EAGAIN) {
                        completed += 1;
                        continue;
                    }
                    return Err(err);
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
