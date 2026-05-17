//! Linked SQE chains for the read -> checksum -> write pipeline.
//!
//! `IOSQE_IO_LINK` lets a caller submit a sequence of SQEs that the kernel
//! executes in order: each subsequent SQE is held back until the previous one
//! completes successfully. The first SQE that fails (or completes short)
//! cancels every subsequent SQE in the chain with `ECANCELED`. This is the
//! single-syscall primitive behind the read-checksum-write fast path: a chain
//! of `IORING_OP_READ` -> `IORING_OP_WRITE` SQEs that the kernel sequences
//! without round-tripping userspace between the ops.
//!
//! # Shape
//!
//! [`LinkedChain`] owns a [`RingLease`] for the duration of the build/submit
//! cycle. SQEs are buffered in a local `Vec` and then submitted as a single
//! batch; on every SQE except the last we set `IOSQE_IO_LINK` so the kernel
//! treats them as a dependency chain. [`submit_and_wait`](LinkedChain::submit_and_wait)
//! drains every CQE that came back and surfaces a [`CqeResult`] per submitted
//! SQE, in chain order. Callers can detect a broken chain by inspecting the
//! per-result `result` field: the first failing op carries the real error, and
//! every op that the kernel cancelled in its wake carries
//! [`libc::ECANCELED`].
//!
//! # Lifetimes
//!
//! Buffers passed to [`read`](LinkedChain::read) and [`write`](LinkedChain::write)
//! are borrowed with the chain's `'r` lifetime. The chain holds the borrow
//! until [`submit_and_wait`](LinkedChain::submit_and_wait) returns, so the
//! kernel never sees a buffer whose owning slice has been dropped or reused.
//!
//! # Upstream reference
//!
//! - `liburing/src/include/liburing/io_uring.h` -- `IOSQE_IO_LINK`
//! - Linux kernel `fs/io_uring.c::io_submit_sqes` for chain semantics
//!   (each link consumes its predecessor's success, `IOSQE_IO_LINK` chains
//!   break on the first short or failed completion).

use std::io;
use std::os::unix::io::RawFd;

use io_uring::{opcode, squeue, types};

use super::session_pool::RingLease;

/// Result of one completion queue entry in a [`LinkedChain`].
///
/// `result` mirrors the kernel's `cqe->res`: non-negative is the byte count
/// returned by the underlying read/write, negative is the negated errno. We
/// keep the raw shape (instead of pre-mapping to `io::Result`) so callers can
/// distinguish a real failure from the chain-break sentinel `-ECANCELED`
/// without losing the original errno.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CqeResult {
    /// Chain position (0-based) of the originating SQE. Equal to the SQE's
    /// user-data tag.
    pub index: u32,
    /// Raw kernel completion result: bytes transferred on success, negated
    /// errno on failure.
    pub result: i32,
}

impl CqeResult {
    /// Returns the byte count on success, or maps the negative result to an
    /// [`io::Error`] on failure.
    pub fn into_io_result(self) -> io::Result<u32> {
        if self.result < 0 {
            Err(io::Error::from_raw_os_error(-self.result))
        } else {
            Ok(self.result as u32)
        }
    }

    /// Returns `true` when this completion is the kernel cancelling a linked
    /// SQE because an earlier link in the chain failed.
    #[must_use]
    pub fn is_chain_cancellation(self) -> bool {
        self.result == -libc::ECANCELED
    }
}

/// One staged SQE in a [`LinkedChain`].
///
/// Stored as a borrowed pointer/length pair plus the metadata needed to
/// rebuild the `io-uring` entry just before submission. We defer the call to
/// `opcode::Read::build` / `opcode::Write::build` so the linked-chain logic
/// can set `IOSQE_IO_LINK` on every entry except the last without juggling a
/// vector of opaque `squeue::Entry` builders that the `io-uring` crate does
/// not expose for inspection.
enum StagedOp<'r> {
    Read {
        fd: RawFd,
        ptr: *mut u8,
        len: u32,
        offset: u64,
        _life: std::marker::PhantomData<&'r mut [u8]>,
    },
    Write {
        fd: RawFd,
        ptr: *const u8,
        len: u32,
        offset: u64,
        _life: std::marker::PhantomData<&'r [u8]>,
    },
}

/// Builder for a sequence of `IOSQE_IO_LINK`-chained SQEs.
///
/// The chain owns a [`RingLease`] for the duration of build + submit + reap;
/// no other consumer can submit on the same ring slot while a chain is live,
/// so the completion queue is guaranteed to contain only this chain's CQEs
/// when [`submit_and_wait`](Self::submit_and_wait) reads them back.
///
/// # Usage
///
/// ```ignore
/// let lease = pool.acquire().expect("ring available");
/// let cqes = LinkedChain::new(lease)
///     .read(src_fd, &mut buf, src_off)
///     .write(dst_fd, &buf, dst_off)
///     .submit_and_wait()?;
/// for cqe in cqes {
///     cqe.into_io_result()?;
/// }
/// ```
pub struct LinkedChain<'r> {
    lease: RingLease<'r>,
    ops: Vec<StagedOp<'r>>,
}

impl<'r> LinkedChain<'r> {
    /// Starts a new empty chain bound to `lease`.
    ///
    /// The lease is held until the chain is consumed by
    /// [`submit_and_wait`](Self::submit_and_wait) (or dropped without
    /// submission, in which case the ring is released with no SQEs ever
    /// pushed).
    #[must_use]
    pub fn new(lease: RingLease<'r>) -> Self {
        Self {
            lease,
            ops: Vec::new(),
        }
    }

    /// Appends an `IORING_OP_READ` to the chain.
    ///
    /// The buffer is borrowed for the chain's lifetime, so it cannot be
    /// reused or dropped until [`submit_and_wait`](Self::submit_and_wait)
    /// returns. `offset` is the file offset to read from; for streamed
    /// reads use `u64::MAX` (kernel `-1`) which the kernel treats as
    /// "current file position".
    #[must_use]
    pub fn read(mut self, fd: RawFd, buf: &'r mut [u8], offset: u64) -> Self {
        // Capture the buffer pointer/length before we drop the &mut borrow.
        // The PhantomData<&'r mut [u8]> in StagedOp::Read keeps the borrow
        // alive at the type level for the chain's lifetime, so the kernel
        // never sees a buffer that has been freed or aliased.
        let len = u32::try_from(buf.len()).unwrap_or(u32::MAX);
        let ptr = buf.as_mut_ptr();
        self.ops.push(StagedOp::Read {
            fd,
            ptr,
            len,
            offset,
            _life: std::marker::PhantomData,
        });
        self
    }

    /// Appends an `IORING_OP_WRITE` to the chain.
    ///
    /// The buffer is borrowed for the chain's lifetime. Like
    /// [`read`](Self::read), `offset = u64::MAX` selects the kernel's
    /// "current file position" semantics.
    #[must_use]
    pub fn write(mut self, fd: RawFd, buf: &'r [u8], offset: u64) -> Self {
        let len = u32::try_from(buf.len()).unwrap_or(u32::MAX);
        let ptr = buf.as_ptr();
        self.ops.push(StagedOp::Write {
            fd,
            ptr,
            len,
            offset,
            _life: std::marker::PhantomData,
        });
        self
    }

    /// Returns the number of SQEs currently staged.
    #[must_use]
    pub fn len(&self) -> usize {
        self.ops.len()
    }

    /// Returns `true` when no SQE has been staged yet.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.ops.is_empty()
    }

    /// Submits every staged SQE as a single linked chain and waits for all
    /// completions.
    ///
    /// Returns the per-SQE completion results in chain order. The chain
    /// breaks on the first failed link; the kernel cancels the rest with
    /// `ECANCELED`, which surfaces as
    /// [`CqeResult::is_chain_cancellation`].
    ///
    /// # Errors
    ///
    /// Returns an `io::Error` for failures that prevent the chain from being
    /// observed at all -- submission-queue overflow, `io_uring_enter`
    /// returning an error, or a missing CQE. Per-link kernel errors are
    /// reported through the returned `Vec<CqeResult>`, not as a top-level
    /// `Err`, so the caller can inspect which links succeeded before the
    /// chain broke.
    pub fn submit_and_wait(mut self) -> io::Result<Vec<CqeResult>> {
        if self.ops.is_empty() {
            return Ok(Vec::new());
        }

        let n = self.ops.len();
        let last = n - 1;

        // SAFETY: Each SQE references a buffer that is borrowed by the
        // chain (via the `'r` lifetime captured in `StagedOp`). The chain
        // holds those borrows for the entire submit-and-wait cycle, so the
        // kernel cannot access a freed or reused buffer. The fd is
        // owned/borrowed by the caller and outlives the call. The
        // `user_data` we attach is the chain index, which lets us attribute
        // CQEs back to their originating SQE when the completion queue
        // reorders them.
        unsafe {
            let mut sq = self.lease.submission();
            for (idx, op) in self.ops.iter().enumerate() {
                let entry = build_entry(op, idx as u64);
                let flags = if idx == last {
                    squeue::Flags::empty()
                } else {
                    squeue::Flags::IO_LINK
                };
                let entry = entry.flags(flags);
                sq.push(&entry)
                    .map_err(|_| io::Error::other("submission queue full"))?;
            }
        }

        self.lease.submit_and_wait(n)?;

        let mut out = Vec::with_capacity(n);
        while let Some(cqe) = self.lease.completion().next() {
            out.push(CqeResult {
                index: cqe.user_data() as u32,
                result: cqe.result(),
            });
        }
        if out.len() < n {
            return Err(io::Error::other(format!(
                "io_uring linked chain: expected {n} CQEs, got {}",
                out.len()
            )));
        }
        out.sort_by_key(|c| c.index);
        Ok(out)
    }
}

fn build_entry(op: &StagedOp<'_>, user_data: u64) -> squeue::Entry {
    match *op {
        StagedOp::Read {
            fd,
            ptr,
            len,
            offset,
            ..
        } => opcode::Read::new(types::Fd(fd), ptr, len)
            .offset(offset)
            .build()
            .user_data(user_data),
        StagedOp::Write {
            fd,
            ptr,
            len,
            offset,
            ..
        } => opcode::Write::new(types::Fd(fd), ptr, len)
            .offset(offset)
            .build()
            .user_data(user_data),
    }
}

// Linked chains are tied to a single ring lease and the buffers they borrow;
// the staged ops carry raw pointers but the `_life` PhantomData ties them to
// the chain's `'r` lifetime, which already prevents Send. We do not add an
// `unsafe impl Send` here on purpose -- chains are single-threaded by design.

/// Convenience wrapper: builds and submits a one-shot read-then-write chain.
///
/// This is the canonical shape for the engine's local-copy fast path:
/// stream a block from `src_fd` straight into a write on `dst_fd`. Returns
/// the bytes transferred by the write SQE on success.
///
/// The same `buf` backs both SQEs. The kernel runs the read fully before
/// touching the write because of `IOSQE_IO_LINK`, so the two SQEs never
/// observe the buffer concurrently. We stage the ops as raw pointers so the
/// borrow checker is not asked to reason about that ordering.
pub fn read_then_write(
    mut lease: RingLease<'_>,
    src_fd: RawFd,
    src_offset: u64,
    dst_fd: RawFd,
    dst_offset: u64,
    buf: &mut [u8],
) -> io::Result<u32> {
    let len = u32::try_from(buf.len()).unwrap_or(u32::MAX);
    let ptr = buf.as_mut_ptr();

    let read_entry = opcode::Read::new(types::Fd(src_fd), ptr, len)
        .offset(src_offset)
        .build()
        .user_data(0)
        .flags(squeue::Flags::IO_LINK);
    let write_entry = opcode::Write::new(types::Fd(dst_fd), ptr.cast_const(), len)
        .offset(dst_offset)
        .build()
        .user_data(1);

    // SAFETY: `buf` is borrowed for the entire call, so the pointer remains
    // valid until both CQEs are drained below. The fds are caller-owned and
    // outlive the call. The read and write reference the same memory but the
    // kernel runs them strictly in order thanks to IOSQE_IO_LINK; only one
    // op touches the buffer at a time.
    unsafe {
        let mut sq = lease.submission();
        sq.push(&read_entry)
            .map_err(|_| io::Error::other("submission queue full"))?;
        sq.push(&write_entry)
            .map_err(|_| io::Error::other("submission queue full"))?;
    }
    lease.submit_and_wait(2)?;

    let mut results: [Option<CqeResult>; 2] = [None, None];
    while let Some(cqe) = lease.completion().next() {
        let idx = cqe.user_data() as usize;
        if idx < results.len() {
            results[idx] = Some(CqeResult {
                index: idx as u32,
                result: cqe.result(),
            });
        }
    }
    // The write CQE carries the bytes-transferred we care about. A failed
    // read cancels the write with ECANCELED, which we surface as an error.
    results[1]
        .ok_or_else(|| io::Error::other("missing write CQE in read_then_write chain"))?
        .into_io_result()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs::OpenOptions;
    use std::io::{Read as _, Seek as _, SeekFrom, Write as _};
    use std::os::unix::io::AsRawFd;

    use crate::io_uring::config::{IoUringConfig, is_io_uring_available};
    use crate::io_uring::session_pool::{SessionPoolConfig, SessionRingPool};

    fn pool_for_tests() -> Option<SessionRingPool> {
        if !is_io_uring_available() {
            return None;
        }
        let cfg = SessionPoolConfig::from_io_uring_config(&IoUringConfig::default());
        SessionRingPool::try_new(cfg)
    }

    #[test]
    fn chain_of_three_reads_returns_correct_data() {
        let Some(pool) = pool_for_tests() else {
            eprintln!("skipping: io_uring unavailable");
            return;
        };
        let mut tmp = tempfile::NamedTempFile::new().expect("tempfile");
        let payload: Vec<u8> = (0..3 * 256).map(|i| (i % 251) as u8).collect();
        tmp.write_all(&payload).expect("write payload");
        let file = OpenOptions::new()
            .read(true)
            .open(tmp.path())
            .expect("reopen");
        let fd = file.as_raw_fd();

        let mut b0 = vec![0u8; 256];
        let mut b1 = vec![0u8; 256];
        let mut b2 = vec![0u8; 256];

        let lease = pool.acquire().expect("ring lease");
        let cqes = LinkedChain::new(lease)
            .read(fd, &mut b0, 0)
            .read(fd, &mut b1, 256)
            .read(fd, &mut b2, 512)
            .submit_and_wait()
            .expect("submit chain");
        assert_eq!(cqes.len(), 3);
        for (i, cqe) in cqes.iter().enumerate() {
            assert_eq!(cqe.index, i as u32);
            assert_eq!(cqe.result, 256, "link {i} should read 256 bytes");
        }
        assert_eq!(&b0, &payload[..256]);
        assert_eq!(&b1, &payload[256..512]);
        assert_eq!(&b2, &payload[512..]);
    }

    #[test]
    fn mixed_read_write_chain_copies_bytes() {
        let Some(pool) = pool_for_tests() else {
            eprintln!("skipping: io_uring unavailable");
            return;
        };
        let mut src = tempfile::NamedTempFile::new().expect("src tempfile");
        let payload: Vec<u8> = (0..512).map(|i| (i % 211) as u8).collect();
        src.write_all(&payload).expect("write src");
        let src_file = OpenOptions::new()
            .read(true)
            .open(src.path())
            .expect("reopen src");
        let dst = tempfile::NamedTempFile::new().expect("dst tempfile");
        let dst_file = OpenOptions::new()
            .write(true)
            .open(dst.path())
            .expect("reopen dst");

        let mut buf = vec![0u8; 512];
        let lease = pool.acquire().expect("ring lease");
        let written = read_then_write(
            lease,
            src_file.as_raw_fd(),
            0,
            dst_file.as_raw_fd(),
            0,
            &mut buf,
        )
        .expect("read+write chain");
        assert_eq!(written, 512);

        let mut verify = Vec::new();
        let mut f = OpenOptions::new()
            .read(true)
            .open(dst.path())
            .expect("reopen verify");
        f.seek(SeekFrom::Start(0)).expect("seek");
        f.read_to_end(&mut verify).expect("read verify");
        assert_eq!(verify, payload);
    }

    #[test]
    fn bad_fd_in_chain_breaks_subsequent_links() {
        let Some(pool) = pool_for_tests() else {
            eprintln!("skipping: io_uring unavailable");
            return;
        };
        let mut src = tempfile::NamedTempFile::new().expect("src tempfile");
        src.write_all(b"hello world").expect("write");
        let src_file = OpenOptions::new()
            .read(true)
            .open(src.path())
            .expect("reopen");
        let good_fd = src_file.as_raw_fd();
        let bad_fd: RawFd = -1;

        let mut b0 = vec![0u8; 4];
        let mut b1 = vec![0u8; 4];
        let mut b2 = vec![0u8; 4];

        let lease = pool.acquire().expect("ring lease");
        let cqes = LinkedChain::new(lease)
            .read(good_fd, &mut b0, 0)
            .read(bad_fd, &mut b1, 0)
            .read(good_fd, &mut b2, 4)
            .submit_and_wait()
            .expect("submit chain");

        assert_eq!(cqes.len(), 3);
        // First link succeeds with 4 bytes.
        assert_eq!(cqes[0].result, 4);
        assert_eq!(&b0, b"hell");
        // Second link fails (bad fd -> EBADF).
        assert!(cqes[1].result < 0);
        assert_eq!(-cqes[1].result, libc::EBADF);
        // Third link is cancelled by the kernel because link 2 failed.
        assert!(cqes[2].is_chain_cancellation(), "got {:?}", cqes[2]);
    }

    #[test]
    fn empty_chain_returns_empty_vec() {
        let Some(pool) = pool_for_tests() else {
            eprintln!("skipping: io_uring unavailable");
            return;
        };
        let lease = pool.acquire().expect("ring lease");
        let cqes = LinkedChain::new(lease).submit_and_wait().expect("submit");
        assert!(cqes.is_empty());
    }

    #[test]
    fn into_io_result_maps_cancellation_to_error() {
        let cqe = CqeResult {
            index: 0,
            result: -libc::ECANCELED,
        };
        assert!(cqe.is_chain_cancellation());
        let err = cqe.into_io_result().unwrap_err();
        assert_eq!(err.raw_os_error(), Some(libc::ECANCELED));
    }
}
