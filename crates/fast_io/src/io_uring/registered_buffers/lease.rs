//! Zero-copy `READ_FIXED` lease over the per-thread registered buffers.
//!
//! The copy-out fixed path (`submit_read_fixed_batch`) reads into a
//! registered buffer and then copies into the caller's slice, which measured
//! slower than a plain `IORING_OP_READ` on buffered I/O. A lease skips that
//! copy: it keeps the slots checked out and lends the consumer the bytes the
//! kernel wrote, returning the slots when the lease drops.

use std::io;
use std::rc::Rc;

use io_uring::IoUring as RawIoUring;
use io_uring::opcode::ReadFixed;

use super::registry::RegisteredBufferGroup;

/// Bytes read by `IORING_OP_READ_FIXED`, borrowed in place from registered
/// buffers.
///
/// Holds one registered slot per chunk. The slots stay checked out, so no
/// other submission can reuse them, until the lease drops. The lease is
/// `!Send` because the slots belong to the creating thread's group.
pub struct FixedReadLease {
    group: Rc<RegisteredBufferGroup>,
    /// `(slot index, filled length)` per chunk, in file order.
    chunks: Vec<(u16, usize)>,
}

impl FixedReadLease {
    /// Total bytes held by the lease; less than requested only at EOF.
    #[must_use]
    pub fn len(&self) -> usize {
        self.chunks.iter().map(|&(_, len)| len).sum()
    }

    /// True when the read hit EOF before returning any byte.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// The leased bytes as contiguous-in-file chunks, one per registered
    /// buffer, in file order.
    pub fn chunks(&self) -> impl Iterator<Item = &[u8]> {
        self.chunks.iter().map(|&(index, len)| {
            // SAFETY: the slot is checked out by this lease, so nothing else
            // writes it; the kernel finished writing `len` bytes before the
            // CQE was reaped; `len <= buffer_size`; and `self.group` keeps the
            // allocation alive for the borrow of `self`.
            unsafe { std::slice::from_raw_parts(self.group.buffers[index as usize], len) }
        })
    }
}

impl Drop for FixedReadLease {
    fn drop(&mut self) {
        for &(index, _) in &self.chunks {
            self.group.return_slot(index);
        }
    }
}

/// Reads up to `len` bytes at `offset` into free slots of `group` with one
/// `READ_FIXED` SQE per slot, resubmitting short reads, and returns them as a
/// [`FixedReadLease`].
///
/// Reads at most `available() * buffer_size` bytes. The lease stops at the
/// first chunk that EOF left short; later slots go back to the group.
///
/// # Errors
///
/// Propagates a negative CQE result or a submission failure. Slots already
/// checked out return to the group when the partial lease drops.
pub(in crate::io_uring) fn read_fixed_lease(
    ring: &mut RawIoUring,
    group: &Rc<RegisteredBufferGroup>,
    fd: io_uring::types::Fd,
    offset: u64,
    len: usize,
) -> io::Result<FixedReadLease> {
    let chunk_size = group.buffer_size();
    let wanted = len.div_ceil(chunk_size).min(group.available());
    let mut lease = FixedReadLease {
        group: Rc::clone(group),
        chunks: Vec::with_capacity(wanted),
    };
    lease
        .chunks
        .extend((0..wanted).map_while(|_| group.checkout_detached().map(|index| (index, 0))));

    let want = |i: usize| chunk_size.min(len - i * chunk_size);
    let mut eof = vec![false; lease.chunks.len()];
    // A pool larger than the submission queue is read in SQ-sized rounds;
    // chunks left out of one round are still short and join the next.
    let sq_capacity = ring.params().sq_entries() as usize;
    loop {
        let mut submitted = 0usize;
        for (i, &(index, done)) in lease.chunks.iter().enumerate() {
            if eof[i] || done >= want(i) {
                continue;
            }
            if submitted == sq_capacity {
                break;
            }
            // SAFETY: `done < want(i) <= buffer_size`, so the target stays
            // inside the registered buffer the kernel validates against.
            let dst = unsafe { group.buffers[index as usize].add(done) };
            let entry = ReadFixed::new(fd, dst, (want(i) - done) as u32, index)
                .offset(offset + (i * chunk_size + done) as u64)
                .build()
                .user_data(i as u64);
            // SAFETY: the slot is exclusively checked out by `lease` and stays
            // allocated and registered until `submit_and_wait` below returns.
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
        ring.submit_and_wait(submitted)?;
        for _ in 0..submitted {
            let cqe = ring
                .completion()
                .next()
                .ok_or_else(|| io::Error::other("missing CQE"))?;
            let i = cqe.user_data() as usize;
            match cqe.result() {
                r if r < 0 => return Err(io::Error::from_raw_os_error(-r)),
                0 => eof[i] = true,
                r => lease.chunks[i].1 += r as usize,
            }
        }
    }

    // Keep the contiguous prefix: everything up to the first chunk EOF left
    // short, which may itself hold a partial tail.
    let keep = (0..lease.chunks.len())
        .find(|&i| lease.chunks[i].1 < want(i))
        .map_or(lease.chunks.len(), |i| {
            i + usize::from(lease.chunks[i].1 > 0)
        });
    for (index, _) in lease.chunks.drain(keep..) {
        group.return_slot(index);
    }
    Ok(lease)
}
