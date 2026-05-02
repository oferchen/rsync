//! Single io_uring ring shared by a reader fd and a writer fd in one session.
//!
//! Background: the per-channel design in [`super::file_reader::IoUringReader`]
//! and [`super::socket_writer::IoUringSocketWriter`] gives every endpoint its
//! own SQ/CQ. Co-locating them on one ring halves the per-op syscall cost
//! (one `io_uring_enter` services both directions) and lets the kernel amortise
//! SQE submission across read and write traffic. This module implements that
//! single-ring topology.
//!
//! # Architecture
//!
//! - One `IoUring` instance per [`SharedRing`].
//! - Both fds are registered into the ring's fixed-file table when supported
//!   (`IORING_REGISTER_FILES`), so SQEs identify them by index instead of by
//!   raw fd.
//! - Reads are submitted via `IORING_OP_READ` against the registered reader
//!   fd. Higher-level batched submitters can take ownership of the registered
//!   buffer pool to use `IORING_OP_READ_FIXED`; the low-level entry points
//!   provided here use the unregistered opcode so the caller may pass any
//!   buffer.
//! - Writes ask the kernel for write-readiness with `IORING_OP_POLL_ADD`
//!   (`POLLOUT`) on the writer fd, then submit `IORING_OP_SEND` once
//!   readiness is signalled. Polling first keeps the writer from blocking
//!   the shared ring on a slow consumer.
//! - Completions are demultiplexed by the SQE `user_data` tag (see
//!   [`OpTag`]). The same loop drains both directions.
//!
//! # SQ / CQ demux scheme
//!
//! Every SQE carries a 64-bit `user_data`:
//!
//! ```text
//!  63        56 55             0
//!  +-----------+----------------+
//!  |  OpTag    |   op_id (56b)  |
//!  +-----------+----------------+
//! ```
//!
//! The tag identifies the originating channel and op kind so the demux loop
//! can route the CQE without per-op state lookups. `op_id` is opaque to the
//! ring; the read and write submitters use it to correlate against per-op
//! buffer slot indices.
//!
//! # Fallback
//!
//! When io_uring is unavailable (kernel < 5.6, seccomp blocks the syscall,
//! the `io_uring` cargo feature is off, or `IORING_OP_POLL_ADD` is not in
//! the kernel's probed opcode set), [`SharedRing::try_new`] returns `None`.
//! Callers must fall back to per-channel rings (the existing
//! [`super::IoUringReader`] / [`super::IoUringSocketWriter`] path) or
//! standard buffered I/O.
//!
//! Even on a successful `try_new`, every individual op also retains a
//! per-channel fallback: a `submit_send` on a kernel buffer that is full
//! returns `EAGAIN` in its CQE result so the caller can re-poll, and the
//! shared ring transparently degrades to raw-fd opcodes when fixed-file
//! registration fails.
//!
//! # Upstream reference
//!
//! Upstream rsync runs reader and writer in the same event loop
//! (`io.c:io_loop`, `io.c:perform_io`) using `select(2)`/`poll(2)` to wait
//! on both fds. This module is the io_uring analogue: one event source
//! servicing both directions on a single CQ.

use std::io;
use std::os::unix::io::RawFd;

use io_uring::{IoUring as RawIoUring, opcode, types};

use super::batching::{NO_FIXED_FD, maybe_fixed_file};
use super::config::{IoUringConfig, is_io_uring_available};
use super::registered_buffers::RegisteredBufferGroup;

/// io_uring opcode value for `IORING_OP_POLL_ADD`.
///
/// Matches the kernel definition in `include/uapi/linux/io_uring.h`. We
/// hard-code the value because [`io_uring::Probe::is_supported`] takes a raw
/// `u8` and the io-uring crate does not expose a `CODE` constant on
/// `opcode::PollAdd` in v0.7.
const IORING_OP_POLL_ADD: u8 = 6;

/// SQE `user_data` tag identifying the source channel of a completion.
///
/// Stored in the high 8 bits of `user_data`. Each value corresponds to a
/// distinct demux arm in [`SharedRing::reap`]. Adding new tags is a backward-
/// compatible change as long as existing values are preserved.
#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OpTag {
    /// File-side `IORING_OP_READ` or `IORING_OP_READ_FIXED` completion.
    Read = 1,
    /// File-side `IORING_OP_WRITE` or `IORING_OP_WRITE_FIXED` completion.
    Write = 2,
    /// Socket-side `IORING_OP_SEND` completion.
    Send = 3,
    /// Write-readiness probe (`IORING_OP_POLL_ADD` with `POLLOUT`).
    PollWrite = 4,
}

impl OpTag {
    /// Encodes the tag and a 56-bit op id into the SQE `user_data` field.
    #[inline]
    #[must_use]
    pub fn encode(self, op_id: u64) -> u64 {
        debug_assert!(op_id < (1 << 56), "op_id {op_id} overflows 56 bits");
        ((self as u64) << 56) | (op_id & ((1u64 << 56) - 1))
    }

    /// Decodes a CQE `user_data` field into the source tag and op id.
    ///
    /// Returns `None` when the high 8 bits do not match a known tag value;
    /// the caller should treat that as a corrupted completion.
    #[inline]
    #[must_use]
    pub fn decode(user_data: u64) -> Option<(Self, u64)> {
        let tag = (user_data >> 56) as u8;
        let op_id = user_data & ((1u64 << 56) - 1);
        let parsed = match tag {
            1 => Self::Read,
            2 => Self::Write,
            3 => Self::Send,
            4 => Self::PollWrite,
            _ => return None,
        };
        Some((parsed, op_id))
    }
}

/// Demultiplexed result of one CQE drained from the shared ring.
///
/// Returned by [`SharedRing::reap`] for each completion the caller pulls off
/// the CQ. The caller decides what to do with each variant - typically the
/// reader path forwards `Read` results to its block matcher, and the writer
/// path uses `PollWrite` to know when to follow up with a `Send`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SharedCompletion {
    /// File read completed; bytes read on success or `-errno` on failure.
    Read {
        /// Caller-supplied op id passed at submission.
        op_id: u64,
        /// Raw CQE result (bytes on success, negative errno on failure).
        result: i32,
    },
    /// File write completed.
    Write {
        /// Caller-supplied op id passed at submission.
        op_id: u64,
        /// Raw CQE result (bytes on success, negative errno on failure).
        result: i32,
    },
    /// Socket send completed.
    Send {
        /// Caller-supplied op id passed at submission.
        op_id: u64,
        /// Raw CQE result (bytes on success, negative errno on failure).
        result: i32,
    },
    /// Write-readiness signalled by the kernel for the registered writer fd.
    ///
    /// The `revents` field carries the `POLLOUT`/`POLLERR`/`POLLHUP` bits
    /// returned by the kernel; the caller should inspect them before issuing
    /// the follow-up send.
    PollWrite {
        /// Caller-supplied op id passed at submission.
        op_id: u64,
        /// `revents` bitmask (raw poll flags from the kernel).
        revents: i16,
    },
}

/// Configuration for a [`SharedRing`].
///
/// Defaults match [`IoUringConfig::default`] for parity with the per-channel
/// rings; callers tuning a session should override via the field below.
#[derive(Debug, Clone, Default)]
pub struct SharedRingConfig {
    /// Backing io_uring configuration (SQ depth, registered buffer count, ...).
    pub ring: IoUringConfig,
}

/// One io_uring ring shared by a reader and a writer fd in the same session.
///
/// See module docs for the architecture and demux scheme. Construction is
/// cheap relative to two per-channel rings: one `io_uring_setup`, one
/// fixed-file table containing both fds, and one optional registered buffer
/// group reused by both directions.
///
/// Drop order: the ring field is declared first, so on drop the kernel ring
/// fd closes before [`RegisteredBufferGroup`] deallocates its user-side
/// pages. This matches the invariant documented in
/// [`super::registered_buffers`].
pub struct SharedRing {
    ring: RawIoUring,
    /// Fixed-file slot index of the reader fd, or `NO_FIXED_FD`.
    reader_slot: i32,
    /// Fixed-file slot index of the writer fd, or `NO_FIXED_FD`.
    writer_slot: i32,
    /// Raw fds kept for the regular (non-fixed) opcode path.
    reader_fd: RawFd,
    writer_fd: RawFd,
    /// Whether `IORING_OP_POLL_ADD` is reported supported by the kernel.
    poll_add_supported: bool,
    /// Optional registered buffer pool shared by READ_FIXED / WRITE_FIXED.
    registered_buffers: Option<RegisteredBufferGroup>,
}

impl SharedRing {
    /// Builds a shared ring for the given reader and writer fds, returning
    /// `None` when io_uring is unavailable or `IORING_OP_POLL_ADD` is not
    /// supported.
    ///
    /// On success, both fds are registered into the ring's fixed-file table
    /// (when `config.ring.register_files` is true). Buffer registration is
    /// best-effort: a registration failure returns `Some(SharedRing)` with
    /// `registered_buffers = None`, and reads/writes silently fall back to
    /// the unregistered opcode variants.
    ///
    /// The caller retains ownership of both fds; this type does not close
    /// them on drop.
    #[must_use]
    pub fn try_new(reader_fd: RawFd, writer_fd: RawFd, config: &SharedRingConfig) -> Option<Self> {
        if !is_io_uring_available() {
            return None;
        }
        Self::new_inner(reader_fd, writer_fd, config).ok()
    }

    /// Builds a shared ring or returns the underlying io_uring construction
    /// error. Use [`try_new`](Self::try_new) for the policy-driven path that
    /// folds availability into a `None` return.
    pub fn new(reader_fd: RawFd, writer_fd: RawFd, config: &SharedRingConfig) -> io::Result<Self> {
        if !is_io_uring_available() {
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "io_uring is not available on this kernel",
            ));
        }
        Self::new_inner(reader_fd, writer_fd, config)
    }

    fn new_inner(
        reader_fd: RawFd,
        writer_fd: RawFd,
        config: &SharedRingConfig,
    ) -> io::Result<Self> {
        let ring = config.ring.build_ring()?;

        // Probe POLL_ADD support: a kernel may meet the 5.6 minimum but still
        // disable specific opcodes (extremely unusual but recorded as the
        // safety net required by the audit in
        // docs/audits/shared-iouring-session-instance.md).
        let poll_add_supported = probe_poll_add(&ring);
        if !poll_add_supported {
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "IORING_OP_POLL_ADD not supported by this kernel",
            ));
        }

        let (reader_slot, writer_slot) = if config.ring.register_files {
            register_pair(&ring, reader_fd, writer_fd)
        } else {
            (NO_FIXED_FD, NO_FIXED_FD)
        };

        let registered_buffers = if config.ring.register_buffers {
            RegisteredBufferGroup::try_new(
                &ring,
                config.ring.buffer_size,
                config.ring.registered_buffer_count,
            )
        } else {
            None
        };

        Ok(Self {
            ring,
            reader_slot,
            writer_slot,
            reader_fd,
            writer_fd,
            poll_add_supported,
            registered_buffers,
        })
    }

    /// Returns whether `IORING_OP_POLL_ADD` was probed as supported when this
    /// ring was constructed. Always `true` for a successfully constructed
    /// ring; exposed for diagnostics and tests.
    #[must_use]
    pub fn poll_add_supported(&self) -> bool {
        self.poll_add_supported
    }

    /// Returns whether the optional registered buffer pool is active.
    #[must_use]
    pub fn has_registered_buffers(&self) -> bool {
        self.registered_buffers.is_some()
    }

    /// Returns the reader fd's fixed-file slot when registered, or
    /// `NO_FIXED_FD` (the sentinel `-1`) when the unregistered fd path is
    /// used. Exposed for diagnostics and integration with batched
    /// submitters that build their own SQEs against the shared ring.
    #[must_use]
    pub fn reader_slot(&self) -> i32 {
        self.reader_slot
    }

    /// Returns the writer fd's fixed-file slot when registered, or
    /// `NO_FIXED_FD` otherwise.
    #[must_use]
    pub fn writer_slot(&self) -> i32 {
        self.writer_slot
    }

    /// Submits a single read against the registered reader fd.
    ///
    /// Uses `IORING_OP_READ` (the unregistered opcode) so the caller's
    /// `buf` may be any slice; the registered-buffer fast path is reserved
    /// for higher-level batched submitters that own the buffer pool.
    ///
    /// # Safety contract
    ///
    /// The caller must ensure `buf` outlives the submission and that no
    /// other reference to the same memory exists until the matching CQE has
    /// been reaped via [`reap`](Self::reap).
    pub fn submit_read(&mut self, op_id: u64, offset: u64, buf: &mut [u8]) -> io::Result<()> {
        let fd = sqe_fd_for(self.reader_fd, self.reader_slot);
        let entry = opcode::Read::new(fd, buf.as_mut_ptr(), buf.len() as u32)
            .offset(offset)
            .build()
            .user_data(OpTag::Read.encode(op_id));
        let entry = maybe_fixed_file(entry, self.reader_slot);
        // SAFETY: `buf` is a live mutable slice provided by the caller; the
        // documented contract requires it to outlive the matching CQE. The
        // SQE is consumed by the kernel only after submit_and_wait().
        unsafe {
            self.ring
                .submission()
                .push(&entry)
                .map_err(|_| io::Error::other("submission queue full"))?;
        }
        Ok(())
    }

    /// Submits an `IORING_OP_POLL_ADD` (`POLLOUT`) on the writer fd.
    ///
    /// Returns once the SQE is queued; the caller drives it to completion
    /// via [`submit_and_wait`](Self::submit_and_wait) + [`reap`](Self::reap).
    /// The matching CQE arrives as [`SharedCompletion::PollWrite`].
    ///
    /// upstream: io.c:perform_io() uses select() to wait for write-readiness
    /// before draining the output buffer; this is the io_uring equivalent.
    pub fn submit_poll_write(&mut self, op_id: u64) -> io::Result<()> {
        let fd = sqe_fd_for(self.writer_fd, self.writer_slot);
        let entry = opcode::PollAdd::new(fd, libc::POLLOUT as u32)
            .build()
            .user_data(OpTag::PollWrite.encode(op_id));
        let entry = maybe_fixed_file(entry, self.writer_slot);
        // SAFETY: PollAdd holds no caller-owned memory; the kernel only
        // dereferences the registered fd, which the caller guarantees to
        // remain valid for the lifetime of this `SharedRing`.
        unsafe {
            self.ring
                .submission()
                .push(&entry)
                .map_err(|_| io::Error::other("submission queue full"))?;
        }
        Ok(())
    }

    /// Submits a stream send on the writer fd. Intended to be paired with a
    /// preceding [`submit_poll_write`](Self::submit_poll_write) when the
    /// caller wants explicit readiness signalling, or used standalone when
    /// the caller knows the kernel buffer has room.
    ///
    /// # Safety contract
    ///
    /// The caller must ensure `data` outlives the submission and that no
    /// mutation occurs until the matching CQE has been reaped.
    pub fn submit_send(&mut self, op_id: u64, data: &[u8]) -> io::Result<()> {
        let fd = sqe_fd_for(self.writer_fd, self.writer_slot);
        let entry = opcode::Send::new(fd, data.as_ptr(), data.len() as u32)
            .build()
            .user_data(OpTag::Send.encode(op_id));
        let entry = maybe_fixed_file(entry, self.writer_slot);
        // SAFETY: `data` is a live shared slice provided by the caller; the
        // contract requires it to outlive the matching CQE.
        unsafe {
            self.ring
                .submission()
                .push(&entry)
                .map_err(|_| io::Error::other("submission queue full"))?;
        }
        Ok(())
    }

    /// Submits queued SQEs and waits for at least `wait_for` completions.
    ///
    /// Wraps [`io_uring::IoUring::submit_and_wait`] so the caller does not
    /// have to import the underlying type.
    pub fn submit_and_wait(&mut self, wait_for: usize) -> io::Result<usize> {
        self.ring.submit_and_wait(wait_for)
    }

    /// Drains every available CQE, returning the demuxed completions in
    /// arrival order.
    ///
    /// Each CQE is decoded via [`OpTag::decode`]; an unknown tag is treated
    /// as a programmer error and surfaces as `Err(InvalidData)` so the
    /// caller can fail loudly rather than silently dropping a completion.
    pub fn reap(&mut self) -> io::Result<Vec<SharedCompletion>> {
        let mut out = Vec::new();
        loop {
            let cqe = match self.ring.completion().next() {
                Some(c) => c,
                None => break,
            };
            let user_data = cqe.user_data();
            let result = cqe.result();
            let (tag, op_id) = OpTag::decode(user_data).ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!("unknown SharedRing user_data tag in CQE: 0x{user_data:016x}"),
                )
            })?;
            let completion = match tag {
                OpTag::Read => SharedCompletion::Read { op_id, result },
                OpTag::Write => SharedCompletion::Write { op_id, result },
                OpTag::Send => SharedCompletion::Send { op_id, result },
                OpTag::PollWrite => SharedCompletion::PollWrite {
                    op_id,
                    revents: result as i16,
                },
            };
            out.push(completion);
        }
        Ok(out)
    }
}

/// Probes the ring for `IORING_OP_POLL_ADD` support.
///
/// Returns `true` when the probe succeeds and the opcode is reported as
/// supported, OR when the probe registration itself fails (which we treat
/// as "kernel does not support `IORING_REGISTER_PROBE`, but POLL_ADD has
/// existed since 5.1, so assume supported"). Returns `false` only when the
/// probe ran and the kernel explicitly excluded the opcode.
fn probe_poll_add(ring: &RawIoUring) -> bool {
    let mut probe = io_uring::Probe::new();
    if ring.submitter().register_probe(&mut probe).is_err() {
        // Probe registration failed (very old kernel or seccomp blocked it).
        // POLL_ADD has been in io_uring since 5.1 and our 5.6 minimum
        // therefore covers it; assume supported.
        return true;
    }
    probe.is_supported(IORING_OP_POLL_ADD)
}

/// Returns the `types::Fd` for an SQE: the fixed-file slot index when
/// registered, otherwise the raw fd.
fn sqe_fd_for(raw_fd: RawFd, fixed_slot: i32) -> types::Fd {
    if fixed_slot != NO_FIXED_FD {
        types::Fd(fixed_slot)
    } else {
        types::Fd(raw_fd)
    }
}

/// Registers `(reader_fd, writer_fd)` into the ring's fixed-file table.
///
/// Returns `(reader_slot, writer_slot)` on success. On failure, returns
/// `(NO_FIXED_FD, NO_FIXED_FD)` so callers transparently fall back to
/// raw-fd opcodes (matching the per-channel ring behaviour).
fn register_pair(ring: &RawIoUring, reader_fd: RawFd, writer_fd: RawFd) -> (i32, i32) {
    let fds = [reader_fd, writer_fd];
    match ring.submitter().register_files(&fds) {
        Ok(()) => (0, 1),
        Err(_) => (NO_FIXED_FD, NO_FIXED_FD),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn op_tag_round_trip_preserves_id_and_kind() {
        for tag in [OpTag::Read, OpTag::Write, OpTag::Send, OpTag::PollWrite] {
            for &op_id in &[0u64, 1, 42, (1u64 << 56) - 1] {
                let encoded = tag.encode(op_id);
                let (decoded_tag, decoded_id) =
                    OpTag::decode(encoded).expect("known tag must decode");
                assert_eq!(decoded_tag, tag);
                assert_eq!(decoded_id, op_id);
            }
        }
    }

    #[test]
    fn op_tag_decode_rejects_unknown_tag() {
        // Tag value 0xff is not in the OpTag enum.
        let user_data = (0xffu64 << 56) | 7;
        assert!(OpTag::decode(user_data).is_none());
    }

    #[test]
    fn op_tag_encoding_keeps_id_in_low_56_bits() {
        let encoded = OpTag::Read.encode(0xdead_beef);
        assert_eq!(encoded & ((1u64 << 56) - 1), 0xdead_beef);
        assert_eq!(encoded >> 56, OpTag::Read as u64);
    }

    #[test]
    fn shared_ring_config_default_matches_io_uring_config_default() {
        let cfg = SharedRingConfig::default();
        let baseline = IoUringConfig::default();
        assert_eq!(cfg.ring.sq_entries, baseline.sq_entries);
        assert_eq!(cfg.ring.buffer_size, baseline.buffer_size);
        assert_eq!(cfg.ring.register_files, baseline.register_files);
    }
}
