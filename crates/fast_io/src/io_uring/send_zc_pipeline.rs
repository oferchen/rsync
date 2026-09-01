//! Pipelined `IORING_OP_SEND_ZC` submission for the socket write path.
//!
//! `IORING_OP_SEND_ZC` posts two completions per submission: a **transfer**
//! CQE ("the bytes are queued for transmit", carrying the byte count and
//! `IORING_CQE_F_MORE` when a notification will follow) and a
//! **notification** CQE (`IORING_CQE_F_NOTIF`, "the kernel has released its
//! reference to your pages").
//!
//! [`super::send_zc::try_send_zc`] waits for both, which makes it a drop-in
//! replacement for `send(2)` but forfeits the entire point of zero-copy: the
//! kernel releases the pages only once the peer has consumed the data, so
//! waiting for the notification couples the sender's rate to the receiver's
//! drain rate and allows exactly one send in flight. Measured on a 200 MB
//! loopback daemon pull (6400 sends of 32776 bytes), the notification wait
//! was 75% of the time spent inside `try_send_zc` and accounted for the bulk
//! of a 4x end-to-end regression against a plain socket write.
//!
//! This module splits the two waits apart:
//!
//! - The **transfer** CQE is still awaited synchronously. It carries the byte
//!   count (so short sends are handled exactly as before) and any `-errno`,
//!   and once it has been observed the bytes are in the socket's transmit
//!   queue - which is what preserves wire ordering against every other writer
//!   on the same fd.
//! - The **notification** CQE is *not* awaited. The buffer it refers to stays
//!   owned by this pipeline, unwritten and unfreed, until the notification
//!   arrives. A pool of [`SEND_ZC_INFLIGHT_BUFFERS`] buffers lets the caller
//!   keep filling a fresh buffer while earlier ones are still pinned, and
//!   bounds the memory an un-reaped notification can hold.
//!
//! # Buffer-lifetime contract
//!
//! Callers hand the pipeline an **owned** buffer, never a borrowed slice:
//! [`SendZcPipeline::send_staged`] swaps the caller's staging buffer for one
//! the kernel has already released. There is therefore no way for a caller to
//! observe, reuse, or free a buffer the kernel still holds. The pinned
//! buffers outlive the submission because the pipeline owns them, and
//! [`SendZcPipeline::drop`] drains every outstanding notification before any
//! of them is deallocated (leaking the still-pinned buffers rather than
//! freeing them if the drain itself fails).
//!
//! # Why a dedicated ring
//!
//! Deferred notifications mean CQEs from this pipeline stay queued across
//! calls. Every other io_uring consumer on the calling thread
//! ([`super::batching::submit_send_batch`] in particular) drains the shared
//! per-thread ring's completion queue unconditionally and demultiplexes by
//! `user_data` alone, so a stray notification left on that ring would be
//! consumed by - and misread as - somebody else's completion. Owning a
//! private ring makes the demux question disappear: nothing else ever submits
//! to or reaps from it. The ring is per socket writer, i.e. per connection,
//! not per file or per operation, so it does not reintroduce the shared-ring
//! serialisation IUR-3 removed.
//!
//! oc-only: upstream rsync has no io_uring path, so there is no upstream
//! behaviour to mirror here.

use std::io;
use std::os::unix::io::RawFd;

use io_uring::{IoUring as RawIoUring, opcode, types};

use super::send_zc::{IORING_CQE_F_MORE, IORING_CQE_F_NOTIF};

/// Number of send buffers the pipeline keeps, and therefore the maximum
/// number of buffers the kernel may hold pinned at once.
///
/// One buffer is enough for correctness but not for throughput: the caller
/// would stall on the previous notification before it could refill. Four
/// covers the measured loopback ratio - a 32 KiB send reaches its transfer
/// CQE in ~12 us but its notification only ~28 us later, so roughly three
/// sends are in flight during one notification window - and caps the
/// pipeline's footprint at `4 * buffer_size` (256 KiB for the daemon's
/// 64 KiB socket buffer) per connection.
pub(super) const SEND_ZC_INFLIGHT_BUFFERS: usize = 4;

/// One pooled send buffer plus the count of submissions referencing it whose
/// page reference the kernel has not released yet.
///
/// `pending` is incremented when an SQE referencing `buf` is pushed and
/// decremented exactly once per submission - either by the notification CQE,
/// or by a transfer CQE with `IORING_CQE_F_MORE` clear, which is the kernel's
/// way of saying no notification will follow. Counting up at submit time
/// keeps `pending` an upper bound at every instant, so a buffer is never
/// considered free while the kernel might still be reading it.
struct PinnedBuffer {
    buf: Vec<u8>,
    pending: u32,
}

/// A private io_uring ring plus a pool of owned send buffers, submitting
/// `IORING_OP_SEND_ZC` without waiting for the notification CQE.
///
/// See the module docs for the buffer-lifetime contract and the reason the
/// ring is not shared.
pub(super) struct SendZcPipeline {
    ring: RawIoUring,
    fd: RawFd,
    slots: Vec<PinnedBuffer>,
}

impl SendZcPipeline {
    /// Builds a pipeline for `fd` with [`SEND_ZC_INFLIGHT_BUFFERS`] buffers of
    /// `buffer_size` bytes each.
    ///
    /// The caller retains ownership of `fd`; the pipeline neither closes nor
    /// duplicates it and MUST be dropped before the fd is closed (the drain in
    /// [`SendZcPipeline::drop`] needs the ring, not the socket, but a closed fd
    /// would make any in-flight submission complete with an error CQE that the
    /// drain must still observe).
    ///
    /// # Errors
    ///
    /// The `io_uring_setup(2)` error when the ring cannot be created. Callers
    /// treat that as "no zero-copy on this writer" and fall back to the plain
    /// send path.
    pub(super) fn new(fd: RawFd, buffer_size: usize) -> io::Result<Self> {
        // One transfer CQE is outstanding at a time and at most
        // SEND_ZC_INFLIGHT_BUFFERS notifications, so a submission queue of
        // 2 * SEND_ZC_INFLIGHT_BUFFERS (whose default completion queue is
        // twice that again) cannot overflow.
        let entries = (2 * SEND_ZC_INFLIGHT_BUFFERS).next_power_of_two() as u32;
        let ring = RawIoUring::new(entries)?;
        let slots = (0..SEND_ZC_INFLIGHT_BUFFERS)
            .map(|_| PinnedBuffer {
                buf: vec![0u8; buffer_size],
                pending: 0,
            })
            .collect();
        Ok(Self { ring, fd, slots })
    }

    /// Sends the first `len` bytes of `staging` and leaves `staging` holding a
    /// buffer whose pages the kernel has already released.
    ///
    /// Blocks until every one of those bytes has been accepted by the kernel
    /// (each submission's transfer CQE), so on return the data is in the
    /// socket's transmit queue and ordered ahead of anything a later writer
    /// submits on the same fd. It does **not** block for the notification
    /// CQEs; the bytes stay in a pipeline-owned buffer until they arrive.
    ///
    /// `staging` and every pooled buffer have the same length, so the swap
    /// leaves the caller with a buffer of unchanged capacity.
    ///
    /// # Errors
    ///
    /// - [`io::ErrorKind::WriteZero`] when the kernel reports a zero-byte
    ///   send, matching the plain send path's treatment of a stalled socket.
    /// - The OS error carried by a transfer CQE, or any `io_uring_enter(2)`
    ///   error. On error the partially-sent buffer stays owned by the
    ///   pipeline and its notifications are still drained on drop.
    pub(super) fn send_staged(&mut self, staging: &mut Vec<u8>, len: usize) -> io::Result<()> {
        let slot = self.acquire_free_slot()?;
        std::mem::swap(staging, &mut self.slots[slot].buf);
        self.send_slot(slot, len)
    }

    /// Blocks until the kernel has released every buffer this pipeline owns.
    ///
    /// # Errors
    ///
    /// Any `io_uring_enter(2)` error while waiting. The caller cannot make the
    /// buffers safe to free after such an error; [`SendZcPipeline::drop`]
    /// leaks them instead.
    pub(super) fn drain(&mut self) -> io::Result<()> {
        let Self { ring, slots, .. } = self;
        while slots.iter().any(|slot| slot.pending > 0) {
            reap_queued(ring, slots);
            if slots.iter().all(|slot| slot.pending == 0) {
                break;
            }
            match ring.submit_and_wait(1) {
                Ok(_) => {}
                // A signal interrupted `io_uring_enter(2)`. Resuming the wait
                // is a syscall restart, not a retry policy: the notification
                // is still coming, and giving up here would force the drop
                // path to leak the buffers it cannot prove are free.
                Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
                Err(e) => return Err(e),
            }
        }
        Ok(())
    }

    /// Number of submissions whose pages the kernel has not released yet.
    ///
    /// Exposed for tests and diagnostics: a non-zero value right after
    /// [`send_staged`](Self::send_staged) returns is what distinguishes this
    /// pipeline from the synchronous
    /// [`try_send_zc`](super::send_zc::try_send_zc).
    #[cfg(test)]
    pub(super) fn pending_notifications(&self) -> u32 {
        self.slots.iter().map(|slot| slot.pending).sum()
    }

    /// Returns the index of a buffer the kernel is not holding, blocking on
    /// notification CQEs when every buffer is pinned.
    fn acquire_free_slot(&mut self) -> io::Result<usize> {
        let Self { ring, slots, .. } = self;
        loop {
            reap_queued(ring, slots);
            if let Some(idx) = slots.iter().position(|slot| slot.pending == 0) {
                return Ok(idx);
            }
            ring.submit_and_wait(1)?;
        }
    }

    /// Submits `len` bytes from `slots[slot]`, resubmitting the remainder on a
    /// short send, and waits only for each submission's transfer CQE.
    fn send_slot(&mut self, slot: usize, len: usize) -> io::Result<()> {
        let Self { ring, fd, slots } = self;
        let mut sent = 0usize;
        while sent < len {
            let remaining = len - sent;
            // SAFETY: `sent < len <= buf.len()`, so the offset stays inside
            // the buffer's allocation and `remaining` bytes from it do too.
            let ptr = unsafe { slots[slot].buf.as_ptr().add(sent) };
            let entry = opcode::SendZc::new(types::Fd(*fd), ptr, remaining as u32)
                .build()
                .user_data(slot as u64);

            slots[slot].pending += 1;
            // SAFETY: `entry` points into `slots[slot].buf`, which this
            // pipeline owns. `pending` was incremented immediately above -
            // before the SQE could complete - and only returns to zero once
            // the kernel has posted the notification CQE for this submission
            // (or told us via a cleared `IORING_CQE_F_MORE` that it will not
            // post one). No caller can observe, refill, or free a buffer with
            // `pending > 0`: `acquire_free_slot` skips it and
            // `SendZcPipeline::drop` drains before any deallocation. The SQE
            // also references `fd`, which the caller keeps open for at least
            // the pipeline's lifetime.
            let pushed = unsafe { ring.submission().push(&entry) };
            if pushed.is_err() {
                slots[slot].pending -= 1;
                return Err(io::Error::other("SEND_ZC submission queue full"));
            }

            let result = wait_for_transfer(ring, slots)?;
            if result < 0 {
                return Err(io::Error::from_raw_os_error(-result));
            }
            if result == 0 {
                return Err(io::Error::new(
                    io::ErrorKind::WriteZero,
                    "SEND_ZC reported zero bytes sent",
                ));
            }
            sent += result as usize;
        }
        Ok(())
    }
}

impl Drop for SendZcPipeline {
    /// Drains every outstanding notification before the pooled buffers are
    /// deallocated.
    ///
    /// This is the only thing standing between a deferred notification and a
    /// use-after-free: the kernel may still be reading pages backing
    /// `self.slots[..].buf`, and dropping a `Vec` frees them. The drain is
    /// unconditional - it runs on the normal path, on the error path, and
    /// during unwinding. If the drain itself fails (an `io_uring_enter(2)`
    /// error leaves us unable to learn when the kernel is done), the still
    /// pinned buffers are leaked with [`std::mem::forget`]: leaking memory is
    /// sound, freeing memory the kernel holds a reference to is not.
    ///
    /// Closing the ring fd is *not* a substitute. io_uring teardown is
    /// deferred to `io_ring_exit_work`, so the kernel's page references may
    /// outlive `close(2)` on the ring.
    fn drop(&mut self) {
        if self.drain().is_ok() {
            return;
        }
        for slot in &mut self.slots {
            if slot.pending > 0 {
                std::mem::forget(std::mem::take(&mut slot.buf));
            }
        }
    }
}

/// Consumes every CQE already queued, updating per-slot notification
/// accounting, and returns the result of the transfer CQE if one was among
/// them.
///
/// At most one submission is outstanding at a time, so at most one transfer
/// CQE can appear in a single drain; every other CQE is a notification for an
/// earlier submission.
fn reap_queued(ring: &mut RawIoUring, slots: &mut [PinnedBuffer]) -> Option<i32> {
    let mut transfer = None;
    for cqe in ring.completion() {
        let Some(slot) = slots.get_mut(cqe.user_data() as usize) else {
            continue;
        };
        if cqe.flags() & IORING_CQE_F_NOTIF != 0 {
            slot.pending = slot.pending.saturating_sub(1);
            continue;
        }
        if cqe.flags() & IORING_CQE_F_MORE == 0 {
            // The kernel is telling us no notification will follow this
            // submission (it failed before any page was pinned), so this
            // transfer CQE is what settles the accounting for it.
            slot.pending = slot.pending.saturating_sub(1);
        }
        transfer = Some(cqe.result());
    }
    transfer
}

/// Blocks until the outstanding submission's transfer CQE has been observed,
/// reaping any notification CQEs that arrive alongside it.
fn wait_for_transfer(ring: &mut RawIoUring, slots: &mut [PinnedBuffer]) -> io::Result<i32> {
    loop {
        if let Some(result) = reap_queued(ring, slots) {
            return Ok(result);
        }
        ring.submit_and_wait(1)?;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Read;
    use std::net::{TcpListener, TcpStream};
    use std::os::unix::io::AsRawFd;
    use std::thread;
    use std::time::Duration;

    /// Payload for the tests whose peer never reads. Sized to fit entirely in
    /// the socket buffers [`widen_socket_buffers`] asks for, so the transfer
    /// CQE cannot block waiting for a receiver that never reads.
    ///
    /// These tests deliberately do **not** assert that the notification is
    /// still outstanding when `send_staged` returns. On loopback it usually is
    /// not: the packet is handed to the receiving socket immediately, so the
    /// kernel releases its reference and posts the notification right behind
    /// the transfer CQE, and a single quiet send reaps both in one drain. What
    /// the pipeline changes is that it never *waits* for the notification -
    /// a property visible only under load, and pinned by the end-to-end
    /// benchmark rather than by a unit test.
    const UNREAD_CHUNK: usize = 128 * 1024;

    /// What the tests request for `SO_SNDBUF`/`SO_RCVBUF`. Linux doubles the
    /// request and caps it at `net.core.{w,r}mem_max` (212992 by default), so
    /// the effective buffer is at least ~416 KiB - comfortably above
    /// [`UNREAD_CHUNK`].
    const WIDE_SOCKET_BUFFER: libc::c_int = 1024 * 1024;

    fn send_zc_unavailable() -> bool {
        !super::super::config::is_io_uring_available() || !super::super::send_zc::is_supported()
    }

    /// Requests oversized send and receive buffers on `fd` so a
    /// [`UNREAD_CHUNK`] payload can sit in the socket with nobody reading it.
    fn widen_socket_buffers(fd: RawFd) {
        for opt in [libc::SO_SNDBUF, libc::SO_RCVBUF] {
            let value = WIDE_SOCKET_BUFFER;
            // SAFETY: `fd` is an open socket owned by the caller for the
            // duration of this call, `value` is a live `c_int` and the length
            // matches its size, which is what SOL_SOCKET/SO_*BUF expect. A
            // rejected request only leaves the default buffer in place.
            unsafe {
                libc::setsockopt(
                    fd,
                    libc::SOL_SOCKET,
                    opt,
                    std::ptr::addr_of!(value).cast(),
                    std::mem::size_of::<libc::c_int>() as libc::socklen_t,
                );
            }
        }
    }

    /// Connected loopback TCP pair, both ends widened, whose receiving end is
    /// *not* being read.
    fn loopback_pair() -> Option<(TcpStream, TcpStream)> {
        let listener = TcpListener::bind("127.0.0.1:0").ok()?;
        widen_socket_buffers(listener.as_raw_fd());
        let addr = listener.local_addr().ok()?;
        let client = TcpStream::connect(addr).ok()?;
        widen_socket_buffers(client.as_raw_fd());
        let (peer, _) = listener.accept().ok()?;
        widen_socket_buffers(peer.as_raw_fd());
        Some((client, peer))
    }

    /// The enabling fact for deferring the notification: the buffer the kernel
    /// is handed is **not** the buffer the caller keeps. `send_staged` swaps
    /// rather than copies, so the caller comes back holding a different
    /// allocation of the same size and can refill it immediately, whatever the
    /// kernel is still doing with the one it took.
    ///
    /// Waiting for the notification would make this swap pointless; not
    /// swapping would make skipping the wait unsound. The assertion is on
    /// pointer identity, which neither depends on timing nor on whether the
    /// kernel chose to pin or copy the pages.
    #[test]
    fn send_staged_hands_the_kernel_a_buffer_the_caller_no_longer_holds() {
        if send_zc_unavailable() {
            println!("skipping: IORING_OP_SEND_ZC unavailable on this host");
            return;
        }
        let Some((client, peer)) = loopback_pair() else {
            println!("skipping: cannot build a loopback pair");
            return;
        };
        let mut pipeline = match SendZcPipeline::new(client.as_raw_fd(), UNREAD_CHUNK) {
            Ok(p) => p,
            Err(e) => {
                println!("skipping: cannot build the SEND_ZC ring ({e})");
                return;
            }
        };

        let mut staging = vec![7u8; UNREAD_CHUNK];
        let submitted = staging.as_ptr();
        pipeline
            .send_staged(&mut staging, UNREAD_CHUNK)
            .expect("SEND_ZC accepted the payload");
        assert_eq!(
            staging.len(),
            UNREAD_CHUNK,
            "the swapped-in buffer keeps the caller's capacity"
        );
        assert!(
            !std::ptr::eq(staging.as_ptr(), submitted),
            "the caller must be left holding a different allocation from the \
             one handed to the kernel"
        );
        assert!(
            pipeline.pending_notifications() <= 1,
            "one submission cannot leave more than one notification pending"
        );

        drop(peer);
        drop(pipeline);
        drop(client);
    }

    /// Round-trips more buffers than the pool holds, which forces
    /// `acquire_free_slot` to block on notifications and recycle. Verifies
    /// both the byte stream and that the drain settles the accounting.
    #[test]
    fn pipeline_recycles_buffers_across_more_sends_than_slots() {
        if send_zc_unavailable() {
            println!("skipping: IORING_OP_SEND_ZC unavailable on this host");
            return;
        }
        let listener = match TcpListener::bind("127.0.0.1:0") {
            Ok(l) => l,
            Err(e) => {
                println!("skipping: cannot bind loopback ({e})");
                return;
            }
        };
        let addr = listener.local_addr().expect("loopback addr");
        const CHUNK: usize = 64 * 1024;
        const ROUNDS: usize = SEND_ZC_INFLIGHT_BUFFERS * 4;

        let reader = thread::spawn(move || {
            let (mut peer, _) = listener.accept().expect("accept");
            peer.set_read_timeout(Some(Duration::from_secs(30)))
                .expect("read timeout");
            let mut got = Vec::with_capacity(CHUNK * ROUNDS);
            let mut tmp = vec![0u8; 16 * 1024];
            while got.len() < CHUNK * ROUNDS {
                match peer.read(&mut tmp) {
                    Ok(0) => break,
                    Ok(n) => got.extend_from_slice(&tmp[..n]),
                    Err(e) => panic!("peer read failed: {e}"),
                }
            }
            got
        });

        let client = TcpStream::connect(addr).expect("connect");
        let mut pipeline = match SendZcPipeline::new(client.as_raw_fd(), CHUNK) {
            Ok(p) => p,
            Err(e) => {
                println!("skipping: cannot build the SEND_ZC ring ({e})");
                return;
            }
        };

        let mut staging = vec![0u8; CHUNK];
        let mut expected = Vec::with_capacity(CHUNK * ROUNDS);
        for round in 0..ROUNDS {
            let fill = (round & 0xff) as u8;
            staging.fill(fill);
            expected.resize(expected.len() + CHUNK, fill);
            pipeline
                .send_staged(&mut staging, CHUNK)
                .expect("SEND_ZC accepted the payload");
            assert_eq!(staging.len(), CHUNK, "recycled buffer keeps its capacity");
        }
        pipeline.drain().expect("drain observes every notification");
        assert_eq!(
            pipeline.pending_notifications(),
            0,
            "drain must leave no buffer pinned"
        );
        // Idempotent: `Drop` calls `drain` again after the explicit drain in
        // `IoUringSocketWriter::drop`, and it must not block on a completion
        // that is never coming.
        pipeline.drain().expect("a settled drain is a no-op");

        drop(pipeline);
        drop(client);
        let received = reader.join().expect("reader thread");
        assert_eq!(received.len(), expected.len());
        assert_eq!(received, expected, "round-tripped bytes must match");
    }

    /// Dropping the pipeline right after a send must run the drain and leave
    /// the peer with every byte, rather than free buffers the kernel could
    /// still be reading. The peer starts reading only once the drop is
    /// already under way.
    #[test]
    fn drop_drains_outstanding_notifications() {
        if send_zc_unavailable() {
            println!("skipping: IORING_OP_SEND_ZC unavailable on this host");
            return;
        }
        let Some((client, mut peer)) = loopback_pair() else {
            println!("skipping: cannot build a loopback pair");
            return;
        };
        let mut pipeline = match SendZcPipeline::new(client.as_raw_fd(), UNREAD_CHUNK) {
            Ok(p) => p,
            Err(e) => {
                println!("skipping: cannot build the SEND_ZC ring ({e})");
                return;
            }
        };
        let mut staging = vec![3u8; UNREAD_CHUNK];
        pipeline
            .send_staged(&mut staging, UNREAD_CHUNK)
            .expect("SEND_ZC accepted the payload");

        let drainer = thread::spawn(move || {
            peer.set_read_timeout(Some(Duration::from_secs(30)))
                .expect("read timeout");
            let mut tmp = vec![0u8; 16 * 1024];
            let mut total = 0usize;
            while total < UNREAD_CHUNK {
                match peer.read(&mut tmp) {
                    Ok(0) => break,
                    Ok(n) => total += n,
                    Err(_) => break,
                }
            }
            total
        });

        drop(pipeline);
        assert_eq!(drainer.join().expect("peer thread"), UNREAD_CHUNK);
        drop(client);
    }
}
