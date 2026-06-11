//! io_uring-based socket writer using `IORING_OP_SEND`.
//!
//! Submissions go through the per-thread io_uring ring established by IUR-3.a
//! ([`super::per_thread_ring::with_ring`]). The writer no longer owns a
//! `RawIoUring` instance; one ring per OS thread is shared across every
//! [`IoUringSocketWriter`] that thread holds, dissolving the cross-thread
//! contention the shared-ring layout imposed on rayon-parallel senders (IUR-2
//! design doc section 1.1, row "`socket_writer` factory" in section 1.2).
//!
//! The SEND_ZC opcode dispatch is preserved: when the `iouring-send-zc` cargo
//! feature is enabled and the running kernel advertises `IORING_OP_SEND_ZC`,
//! payloads at or above [`SEND_ZC_MIN_BYTES`] are submitted through the
//! zero-copy primitive on the per-thread ring; sub-threshold payloads stay on
//! regular `IORING_OP_SEND`.
//!
//! Per-ring kernel state - fixed-file registration in particular - is tied to
//! a specific ring fd and does not survive the move to a thread-shared ring.
//! It is recorded as unavailable on every writer constructed through this
//! module; IUR-3.e re-introduces ring-bound state on the per-thread topology
//! via the bgid-lease primitive.

use std::io::{self, Write};
use std::os::unix::io::RawFd;

use super::batching::{NO_FIXED_FD, sqe_fd, submit_send_batch};
use super::config::IoUringConfig;
use super::per_thread_ring::with_ring;
use super::send_zc;

/// Payload-length floor below which `IORING_OP_SEND_ZC` is skipped in favour
/// of regular `IORING_OP_SEND`.
///
/// SEND_ZC pins the user pages via `get_user_pages_fast` and waits for the
/// notification CQE before reusing the slot; for sub-page sends the
/// page-pin overhead dominates and SEND_ZC loses to plain SEND.
///
/// The default 16 KiB threshold comes from `docs/design/iouring-send-zc.md`
/// section 5 (workload B regression guard). With the `iouring-send-zc`
/// cargo feature enabled the threshold drops to
/// [`send_zc::SEND_ZC_DISPATCH_MIN_BYTES`] (4 KiB) so the opt-in transport
/// dispatch matches the user-facing contract documented on
/// [`crate::ZeroCopySender`].
#[cfg(not(feature = "iouring-send-zc"))]
const SEND_ZC_MIN_BYTES: usize = 16 * 1024;
#[cfg(feature = "iouring-send-zc")]
const SEND_ZC_MIN_BYTES: usize = send_zc::SEND_ZC_DISPATCH_MIN_BYTES;

/// io_uring-based socket writer using `IORING_OP_SEND` (and optionally
/// `IORING_OP_SEND_ZC`).
///
/// Replaces direct `write()` calls on `TcpStream` by batching sends through
/// the calling thread's per-thread io_uring ring (see
/// [`super::per_thread_ring`]). Maintains an internal write buffer, flushing
/// via batched send SQEs.
///
/// When the [`iouring-send-zc`](send_zc) feature is enabled and the running
/// kernel advertises `IORING_OP_SEND_ZC` (Linux 6.0+), the big-write fast
/// path switches to the zero-copy primitive. Sub-page writes stay on regular
/// `IORING_OP_SEND` because SEND_ZC's page-pin overhead loses to SEND for
/// small payloads. The kernel probe in [`send_zc::is_supported`] is cached
/// process-wide; an unsupported result silently degrades to SEND so the
/// writer never blocks startup on the probe outcome.
///
/// Per-ring fixed-file registration is disabled on this writer until IUR-3.e
/// wires the per-thread registration lease; SQEs always reference the raw
/// socket fd.
pub struct IoUringSocketWriter {
    fd: RawFd,
    buffer: Vec<u8>,
    buffer_pos: usize,
    buffer_size: usize,
    sq_entries: u32,
    /// True when the configured policy permits SEND_ZC **and** the running
    /// kernel advertises `IORING_OP_SEND_ZC`. Resolved once at construction
    /// so the hot path never re-probes.
    send_zc_active: bool,
}

impl IoUringSocketWriter {
    /// Creates a socket writer from a raw file descriptor.
    ///
    /// The caller must ensure `fd` remains valid for the lifetime of this writer.
    /// The writer does NOT take ownership of the fd - it will not close it on drop.
    ///
    /// # Errors
    ///
    /// Returns an error when the per-thread io_uring ring cannot be
    /// constructed on the calling thread (kernel pre-5.6, seccomp-blocked, or
    /// `io_uring_setup(2)` rejection).
    pub fn from_raw_fd(fd: RawFd, config: &IoUringConfig) -> io::Result<Self> {
        // Probe the per-thread ring once at construction so callers observe
        // io_uring unavailability synchronously, matching the old behaviour
        // where `config.build_ring()?` surfaced setup errors here.
        with_ring(|_| Ok(()))?;
        let send_zc_active = config.allow_send_zc() && send_zc::is_supported();

        Ok(Self {
            fd,
            buffer: vec![0u8; config.buffer_size],
            buffer_pos: 0,
            buffer_size: config.buffer_size,
            sq_entries: config.sq_entries,
            send_zc_active,
        })
    }

    /// Returns `true` when this writer will attempt `IORING_OP_SEND_ZC` for
    /// payloads at or above `SEND_ZC_MIN_BYTES`. Exposed for diagnostics
    /// and tests; the production hot path consults the field directly.
    #[must_use]
    pub fn send_zc_active(&self) -> bool {
        self.send_zc_active
    }

    /// Submits `data` via SEND_ZC when policy + kernel + payload size all
    /// agree, falling back to the batched SEND path on `Unsupported` or
    /// when SEND_ZC is disabled. Returns the byte count actually sent.
    fn submit_send(&mut self, data: &[u8]) -> io::Result<usize> {
        if self.send_zc_active && data.len() >= SEND_ZC_MIN_BYTES {
            let fd = self.fd;
            // Wrap the inner result so the `Unsupported` branch can disable
            // SEND_ZC on this writer without propagating as an io_uring
            // ring-construction failure from `with_ring`.
            let outcome: Result<usize, ()> =
                with_ring(|ring| match send_zc::try_send_zc(ring, fd, data, 0) {
                    Ok(n) => Ok(Ok(n)),
                    Err(e) if e.kind() == io::ErrorKind::Unsupported => Ok(Err(())),
                    Err(e) => Err(e),
                })?;
            match outcome {
                Ok(n) => return Ok(n),
                Err(()) => {
                    // Stop trying SEND_ZC for the lifetime of this writer:
                    // the kernel cache already memoised the negative probe,
                    // but turning off the per-writer flag avoids a futile
                    // syscall on every subsequent flush.
                    self.send_zc_active = false;
                }
            }
        }
        let raw_fd = self.fd;
        let fd = sqe_fd(raw_fd, NO_FIXED_FD);
        let buffer_size = self.buffer_size;
        let sq_entries = self.sq_entries as usize;
        with_ring(|ring| submit_send_batch(ring, fd, data, buffer_size, sq_entries, NO_FIXED_FD))
    }

    /// Flushes the internal buffer via SEND_ZC or batched SEND submissions.
    fn flush_buffer(&mut self) -> io::Result<()> {
        if self.buffer_pos == 0 {
            return Ok(());
        }

        let len = self.buffer_pos;
        let mut sent_total = 0;
        while sent_total < len {
            // SAFETY: `as_ptr()` returns a pointer into `self.buffer`, whose
            // backing storage lives until `self` is dropped. The slice
            // bounds are `sent_total..len`, both bounded by
            // `self.buffer_size`. We rebuild the slice on each iteration so
            // it does not alias the `&mut self` borrow consumed by
            // `submit_send`. SEND_ZC's `try_send_zc` waits for the
            // notification CQE before returning, so the kernel has released
            // its page reference by the time we regain control; SEND
            // copies bytes into kernel socket buffers synchronously and is
            // similarly safe to call on a borrowed slice.
            let chunk = unsafe {
                std::slice::from_raw_parts(self.buffer.as_ptr().add(sent_total), len - sent_total)
            };
            let n = self.submit_send(chunk)?;
            if n == 0 {
                return Err(io::Error::new(
                    io::ErrorKind::WriteZero,
                    "batched send incomplete",
                ));
            }
            sent_total += n;
        }

        self.buffer_pos = 0;
        Ok(())
    }
}

impl Write for IoUringSocketWriter {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        if buf.is_empty() {
            return Ok(0);
        }

        if self.buffer_pos + buf.len() <= self.buffer_size {
            self.buffer[self.buffer_pos..self.buffer_pos + buf.len()].copy_from_slice(buf);
            self.buffer_pos += buf.len();
            return Ok(buf.len());
        }

        self.flush_buffer()?;

        if buf.len() >= self.buffer_size {
            // Caller's slice is borrowed for the lifetime of this call;
            // `submit_send` either copies via SEND or waits for the SEND_ZC
            // notification CQE before returning, so the caller is free to
            // reuse `buf` once we return.
            let sent = self.submit_send(buf)?;
            return Ok(sent);
        }

        self.buffer[..buf.len()].copy_from_slice(buf);
        self.buffer_pos = buf.len();
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        self.flush_buffer()
    }
}

impl Drop for IoUringSocketWriter {
    fn drop(&mut self) {
        let _ = self.flush_buffer();
    }
}
