//! `IORING_OP_SEND_ZC` zero-copy socket-send primitive (Linux 6.0+).
//!
//! A regular `IORING_OP_SEND` posts one CQE per submission: the byte count or
//! `-errno`. `IORING_OP_SEND_ZC` posts **two CQEs** per submission:
//!
//! 1. **Transfer CQE** - posted when the data has been queued for transmit.
//!    `IORING_CQE_F_MORE` is set in `flags()` to signal "more CQEs with this
//!    `user_data` will follow", and `result()` carries the byte count (or
//!    `-errno`) exactly like a regular SEND.
//! 2. **Notification CQE** - posted once the kernel has released its
//!    reference to the user pages. `IORING_CQE_F_NOTIF` is set in `flags()`;
//!    `result()` is unused.
//!
//! The buffer passed to a SEND_ZC submission must remain valid and unmodified
//! until the notification CQE arrives. This module enforces that contract by
//! blocking on both CQEs before returning. Callers therefore see SEND_ZC as
//! a synchronous primitive even though the kernel performs the page release
//! asynchronously.
//!
//! See `docs/design/iouring-send-zc.md` for the full design.
//!
//! # `ZeroCopySender`
//!
//! [`ZeroCopySender`] is the higher-level wrapper exposed when the
//! `iouring-send-zc` cargo feature is enabled. It wraps an existing socket
//! fd, an `Arc<Mutex<IoUring>>` ring, and an optional
//! [`RegisteredBufferGroup`] so payload pages can be DMA'd directly from
//! pinned kernel-registered memory without an extra userspace copy.
//!
//! See the [`ZeroCopySender::send_zc`] rustdoc for the buffer-lifetime
//! contract (the kernel does not release its page reference until the
//! notification CQE arrives; the wrapper blocks for that CQE before
//! returning so callers may reuse or drop the slice immediately).

use std::io;
use std::os::unix::io::RawFd;
use std::sync::atomic::{AtomicI8, Ordering};

use io_uring::{IoUring as RawIoUring, opcode, types};

#[cfg(feature = "iouring-send-zc")]
use std::sync::{Arc, Mutex};

#[cfg(feature = "iouring-send-zc")]
use super::config::IoUringConfig;
#[cfg(feature = "iouring-send-zc")]
use super::registered_buffers::RegisteredBufferGroup;

/// CQE flag set on the transfer CQE; signals a notification CQE will follow.
const IORING_CQE_F_MORE: u32 = 1 << 1;

/// CQE flag set on the notification CQE.
const IORING_CQE_F_NOTIF: u32 = 1 << 3;

/// Sentinel `user_data` mask used to keep the SEND_ZC submission from
/// colliding with other CQEs draining on the same ring.
///
/// Callers pass any 56-bit value; the high byte is reserved for a future
/// `OpTag::SendZc` tag. Keeping the encoding internal lets the probe-helper
/// route both CQEs through the same `user_data` match.
const USER_DATA_MASK: u64 = (1u64 << 56) - 1;

/// Cached `IORING_OP_SEND_ZC` support, populated lazily by [`is_supported`].
///
/// Three-state: `0` = not yet probed, `1` = supported, `-1` = unsupported.
static SEND_ZC_SUPPORTED: AtomicI8 = AtomicI8::new(0);

/// Returns `true` when the running kernel advertises `IORING_OP_SEND_ZC`.
///
/// Uses `IORING_REGISTER_PROBE` on a throwaway 4-entry ring, mirroring the
/// `count_supported_ops` cache in `super::config`. Distros backport features
/// and container runtimes lie about kernel versions, so the opcode probe is
/// preferred over a `uname()` floor check.
///
/// The result is cached in a process-wide atomic so subsequent calls are a
/// single relaxed load.
#[must_use]
pub fn is_supported() -> bool {
    match SEND_ZC_SUPPORTED.load(Ordering::Relaxed) {
        1 => return true,
        -1 => return false,
        _ => {}
    }

    let supported = probe_send_zc();
    SEND_ZC_SUPPORTED.store(if supported { 1 } else { -1 }, Ordering::Relaxed);
    supported
}

/// Kernel-version floor for `IORING_OP_SEND_ZC`.
///
/// The opcode landed in Linux 6.0. A handful of 5.x vendor backports advertise
/// the opcode bit through `IORING_REGISTER_PROBE` but ship incomplete
/// zero-copy semantics (the notification-CQE path or page release misbehaves),
/// which corrupts the payload instead of cleanly erroring. The opcode probe
/// alone therefore false-positives on those kernels, so the probe additionally
/// requires the mainline version floor. Requiring BOTH the floor AND the opcode
/// bit means a genuinely-6.0+ kernel that advertises the opcode is the only
/// configuration that ever submits a real SEND_ZC SQE.
const SEND_ZC_KERNEL_MIN: (u32, u32) = (6, 0);

/// Returns `true` when the running kernel is at least [`SEND_ZC_KERNEL_MIN`].
///
/// When the release string cannot be read or parsed, returns `false` so the
/// dispatch conservatively falls back to plain `IORING_OP_SEND`.
fn kernel_meets_send_zc_floor() -> bool {
    super::config::config_detail::get_kernel_release_string()
        .as_deref()
        .and_then(super::config::parse_kernel_version)
        .map(|(major, minor)| (major, minor) >= SEND_ZC_KERNEL_MIN)
        .unwrap_or(false)
}

/// One-shot probe: enforce the 6.0 version floor, then build a tiny ring,
/// register a probe, and check the opcode bit.
///
/// Returns `false` whenever the kernel is below the floor, rejects the probe
/// entirely, or does not advertise the opcode - the caller treats all three as
/// "unsupported" because they surface as `io::ErrorKind::Unsupported` in the
/// fall-back path. Diagnostic separation lives in the existing
/// `config_detail::io_uring_kernel_info` reporter.
fn probe_send_zc() -> bool {
    // Version floor first: it defends against 5.x backports that advertise the
    // opcode bit but ship broken zero-copy semantics (see SEND_ZC_KERNEL_MIN).
    if !kernel_meets_send_zc_floor() {
        return false;
    }
    let Ok(ring) = RawIoUring::new(4) else {
        return false;
    };
    let mut probe = io_uring::Probe::new();
    if ring.submitter().register_probe(&mut probe).is_err() {
        return false;
    }
    probe.is_supported(opcode::SendZc::CODE)
}

/// Submits one `IORING_OP_SEND_ZC` SQE and drains both CQEs synchronously.
///
/// Returns the byte count from the transfer CQE on success. Errors:
///
/// - `io::ErrorKind::Unsupported` if the running kernel does not advertise
///   `IORING_OP_SEND_ZC` (see [`is_supported`]).
/// - `io::ErrorKind::InvalidInput` if `buf` is empty (matches `send(2)`
///   semantics: SEND_ZC with `len = 0` is documented as
///   implementation-defined and is not exercised by any production caller).
/// - The OS error from the transfer CQE if the kernel reports a negative
///   result.
///
/// `user_data` is masked to 56 bits internally; the high byte is reserved
/// for a future `OpTag::SendZc` tag.
///
/// # Invariants
///
/// The function does not return until both the transfer CQE
/// (`IORING_CQE_F_MORE` set) and the notification CQE (`IORING_CQE_F_NOTIF`
/// set) have been observed. By the time the caller regains control, the
/// kernel has released its reference to `buf` and the slice may be reused
/// or dropped immediately.
pub fn try_send_zc(
    ring: &mut RawIoUring,
    fd: RawFd,
    buf: &[u8],
    user_data: u64,
) -> io::Result<usize> {
    if buf.is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "SEND_ZC requires a non-empty buffer",
        ));
    }
    if !is_supported() {
        return Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "IORING_OP_SEND_ZC is not supported on this kernel",
        ));
    }

    let tagged = user_data & USER_DATA_MASK;
    let entry = opcode::SendZc::new(types::Fd(fd), buf.as_ptr(), buf.len() as u32)
        .build()
        .user_data(tagged);

    // SAFETY: `buf` is borrowed for the full lifetime of this call. We drain
    // both CQEs (transfer + notification) below before returning, so the
    // kernel is guaranteed to have released the page reference by the time
    // the caller regains control. The SQE references no other memory.
    unsafe {
        ring.submission()
            .push(&entry)
            .map_err(|_| io::Error::other("submission queue full"))?;
    }

    let mut transfer_result: Option<i32> = None;
    let mut saw_notification = false;

    while transfer_result.is_none() || !saw_notification {
        // Block for at least one CQE if the queue is empty; otherwise drain
        // what's already queued before re-entering the kernel.
        if ring.completion().is_empty() {
            ring.submit_and_wait(1)?;
        }

        while let Some(cqe) = ring.completion().next() {
            if cqe.user_data() != tagged {
                // CQE belongs to an unrelated SQE on the same ring; ignore
                // it. Real callers route those through their own demux.
                continue;
            }
            classify_cqe(
                cqe.result(),
                cqe.flags(),
                &mut transfer_result,
                &mut saw_notification,
            );
            if transfer_result.is_some() && saw_notification {
                break;
            }
        }
    }

    let result = transfer_result.expect("transfer CQE present once loop exits");
    if result < 0 {
        return Err(io::Error::from_raw_os_error(-result));
    }
    Ok(result as usize)
}

/// Classifies a SEND_ZC CQE into the transfer or notification slot.
///
/// Splitting this off keeps [`try_send_zc`] readable and gives the unit
/// tests a tiny pure function to assert against without spinning up a real
/// ring. The function takes raw `result` and `flags` fields so callers can
/// route through any CQE shape (32-byte or 16-byte) without coupling to a
/// concrete entry type.
fn classify_cqe(
    result: i32,
    flags: u32,
    transfer_result: &mut Option<i32>,
    saw_notification: &mut bool,
) {
    if flags & IORING_CQE_F_NOTIF != 0 {
        *saw_notification = true;
        return;
    }
    // The transfer CQE has `IORING_CQE_F_MORE` set when a notification will
    // follow. If the kernel posts a transfer error without a notification
    // (e.g., EBADF before any data is queued), `IORING_CQE_F_MORE` is
    // cleared and there is no second CQE; record the result and synthesise
    // the missing notification so the wait loop terminates.
    *transfer_result = Some(result);
    if flags & IORING_CQE_F_MORE == 0 {
        *saw_notification = true;
    }
}

/// Minimum payload size that the high-level [`ZeroCopySender`] dispatch
/// considers worth routing through `IORING_OP_SEND_ZC`.
///
/// Sub-page sends are dominated by the page-pin overhead of
/// `get_user_pages_fast` and lose to plain `IORING_OP_SEND` (and even to
/// `send(2)`). 4 KiB matches the page size on every supported architecture
/// and is the threshold the transport dispatch uses when the
/// `iouring-send-zc` feature is enabled.
#[cfg(feature = "iouring-send-zc")]
pub const SEND_ZC_DISPATCH_MIN_BYTES: usize = 4 * 1024;

/// Default registered-buffer slot size for [`ZeroCopySender`].
///
/// Sized to match the upstream-rsync 256 KiB literal-token chunk so a
/// single SEND_ZC submission can carry one literal token end-to-end. Larger
/// payloads fall back to the unregistered SEND_ZC path which still skips
/// the userland copy but does not benefit from the pinned-page registration.
#[cfg(feature = "iouring-send-zc")]
const ZERO_COPY_SLOT_BYTES: usize = 256 * 1024;

/// Default number of registered slots in [`ZeroCopySender`]'s buffer pool.
///
/// Eight slots match the engine's `BufferPool` default and the
/// [`IoUringConfig::registered_buffer_count`] default; a single SEND_ZC
/// submission needs one slot for the duration of the call so the pool
/// only needs enough headroom for the brief window between the transfer
/// CQE and the notification CQE.
#[cfg(feature = "iouring-send-zc")]
const ZERO_COPY_SLOT_COUNT: usize = 8;

/// High-level zero-copy socket sender backed by an `Arc<Mutex<IoUring>>`.
///
/// Wraps an existing socket file descriptor and submits
/// `IORING_OP_SEND_ZC` against a [`RegisteredBufferGroup`] so the kernel
/// can DMA payload pages directly without a userspace copy. When
/// registration fails (kernel limit, seccomp) or the payload exceeds the
/// per-slot size, the sender falls back to the unregistered SEND_ZC path
/// which still drains both CQEs and is still zero-copy at the socket layer
/// but does not benefit from pinned-page registration.
///
/// # Buffer-lifetime contract
///
/// `IORING_OP_SEND_ZC` posts the notification CQE only after the kernel
/// has released its reference to the user pages. **The buffer passed to a
/// submission must therefore outlive the kernel hold.** This wrapper
/// upholds that contract by blocking on both CQEs (transfer +
/// notification) before [`send_zc`](Self::send_zc) returns; callers may
/// reuse or drop the slice immediately on return.
///
/// The wrapper is `!Sync` by construction (the `Arc<Mutex<IoUring>>`
/// serialises submissions). Multiple senders may share the same ring via
/// `Arc::clone`; concurrent callers will be serialised on the mutex.
///
/// Only available when the `iouring-send-zc` cargo feature is enabled
/// (Linux + `io_uring` only).
#[cfg(feature = "iouring-send-zc")]
pub struct ZeroCopySender {
    ring: Arc<Mutex<RawIoUring>>,
    fd: RawFd,
    /// Pinned registered-buffer pool. `None` when registration was rejected
    /// by the kernel; the unregistered SEND_ZC path is still usable.
    buffers: Option<RegisteredBufferGroup>,
    /// Per-slot byte capacity. Cached so [`send_zc`](Self::send_zc) can
    /// decide whether the user's payload fits in a single registered slot.
    slot_bytes: usize,
}

#[cfg(feature = "iouring-send-zc")]
impl ZeroCopySender {
    /// Wraps an existing socket `fd` with a freshly-built io_uring ring and a
    /// registered-buffer pool sized for upstream-rsync's 256 KiB literal
    /// chunk.
    ///
    /// The caller retains ownership of the file descriptor; this wrapper
    /// neither closes it on drop nor duplicates it. Callers MUST keep `fd`
    /// open for the lifetime of the sender.
    ///
    /// # Errors
    ///
    /// - [`io::ErrorKind::Unsupported`] when `IORING_OP_SEND_ZC` is not
    ///   advertised by the running kernel.
    /// - The underlying io_uring error when ring construction fails.
    pub fn new(fd: RawFd) -> io::Result<Self> {
        if !is_supported() {
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "IORING_OP_SEND_ZC is not supported on this kernel",
            ));
        }
        let ring = IoUringConfig::default().build_ring()?;
        let buffers =
            RegisteredBufferGroup::try_new(&ring, ZERO_COPY_SLOT_BYTES, ZERO_COPY_SLOT_COUNT);
        let slot_bytes = buffers
            .as_ref()
            .map(RegisteredBufferGroup::buffer_size)
            .unwrap_or(ZERO_COPY_SLOT_BYTES);
        Ok(Self {
            ring: Arc::new(Mutex::new(ring)),
            fd,
            buffers,
            slot_bytes,
        })
    }

    /// Submits `buf` via `IORING_OP_SEND_ZC` and waits for both the
    /// transfer and notification CQEs before returning.
    ///
    /// Returns the byte count reported by the transfer CQE (may be less
    /// than `buf.len()` if the kernel reports a short send; callers loop
    /// over the remaining slice exactly as with `send(2)`).
    ///
    /// # Buffer-lifetime contract
    ///
    /// The zero-copy contract is that `buf` (or, when a registered slot
    /// is used, the per-slot pinned page) must outlive the kernel page
    /// hold. This method enforces the contract by blocking on the
    /// notification CQE before returning, so `buf` is free to be reused
    /// or dropped immediately on return. Callers MUST NOT rely on async
    /// buffer release - that would require a separate API surface that
    /// returns ownership of the buffer along with a completion handle.
    ///
    /// # Errors
    ///
    /// - [`io::ErrorKind::InvalidInput`] when `buf` is empty.
    /// - [`io::ErrorKind::Unsupported`] when the running kernel does not
    ///   advertise `IORING_OP_SEND_ZC` (defensive double-check; the
    ///   constructor already returns `Unsupported` in that case).
    /// - Any OS error reported by the transfer CQE.
    pub fn send_zc(&mut self, buf: &[u8]) -> io::Result<usize> {
        if buf.is_empty() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "SEND_ZC requires a non-empty buffer",
            ));
        }

        // Fast path: payload fits in a registered slot. Copy the bytes
        // into the pinned slot once, then SEND_ZC against the registered
        // memory so the kernel can DMA without another userland touch.
        if let Some(pool) = self.buffers.as_ref() {
            if buf.len() <= self.slot_bytes {
                if let Some(mut slot) = pool.checkout() {
                    // SAFETY: the slot is exclusively ours for the lifetime
                    // of `slot` and `buf.len() <= self.slot_bytes` keeps
                    // the copy inside the slot's registered region. No
                    // concurrent kernel use of this slot is in flight - the
                    // previous send has already drained both CQEs
                    // (try_send_zc is synchronous) before we returned to
                    // the caller.
                    unsafe {
                        let dst = slot.as_mut_slice(buf.len());
                        dst.copy_from_slice(buf);
                    }
                    let mut ring = self
                        .ring
                        .lock()
                        .map_err(|_| io::Error::other("ZeroCopySender ring mutex poisoned"))?;
                    // SAFETY: `slot` is borrowed for the full lifetime of
                    // this call. `try_send_zc` drains both the transfer
                    // and notification CQEs before returning, so the
                    // kernel has released its reference to the registered
                    // pages by the time we drop the slot back to the pool.
                    return try_send_zc(&mut ring, self.fd, unsafe { slot.as_slice(buf.len()) }, 0);
                }
            }
        }

        // Fallback: oversized payload, or registration was rejected.
        // The unregistered SEND_ZC path still skips the userland copy at
        // the socket layer; only the per-page pinning benefit is lost.
        let mut ring = self
            .ring
            .lock()
            .map_err(|_| io::Error::other("ZeroCopySender ring mutex poisoned"))?;
        try_send_zc(&mut ring, self.fd, buf, 0)
    }

    /// Returns `true` when the registered-buffer pool is live (kernel
    /// accepted `IORING_REGISTER_BUFFERS`). Exposed for diagnostics and
    /// tests; production callers should not branch on this.
    #[must_use]
    pub fn registered_buffers_active(&self) -> bool {
        self.buffers.is_some()
    }

    /// Returns the per-slot capacity of the registered buffer pool, or
    /// the configured default when registration was rejected.
    #[must_use]
    pub fn slot_bytes(&self) -> usize {
        self.slot_bytes
    }

    /// Returns the wrapped raw socket file descriptor. The wrapper does
    /// not close the fd on drop; callers retain ownership.
    #[must_use]
    pub fn raw_fd(&self) -> RawFd {
        self.fd
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

    use super::super::config::IoUringConfig;

    #[test]
    fn probe_cache_is_stable() {
        let first = is_supported();
        let second = is_supported();
        assert_eq!(first, second);
    }

    // The version floor is a hard precondition: if `is_supported()` reports the
    // opcode as usable, the running kernel MUST be >= 6.0. This is what keeps a
    // 5.x backport that advertises the opcode bit from ever submitting a real
    // SEND_ZC SQE (the corruption seen on the ubuntu-22.04 ~5.15 CI cell).
    #[test]
    fn supported_implies_kernel_floor_met() {
        if is_supported() {
            assert!(
                kernel_meets_send_zc_floor(),
                "SEND_ZC reported supported but the kernel is below the 6.0 floor"
            );
        }
    }

    // Boundary semantics of the tuple comparison used by the floor: 6.0 and
    // newer pass; anything 5.x or older fails. Mirrors the `(major, minor) >=
    // SEND_ZC_KERNEL_MIN` check without depending on the host's real kernel.
    #[test]
    fn kernel_floor_tuple_comparison_boundaries() {
        assert!((6u32, 0u32) >= SEND_ZC_KERNEL_MIN);
        assert!((6u32, 1u32) >= SEND_ZC_KERNEL_MIN);
        assert!((7u32, 0u32) >= SEND_ZC_KERNEL_MIN);
        assert!((5u32, 15u32) < SEND_ZC_KERNEL_MIN);
        assert!((5u32, 19u32) < SEND_ZC_KERNEL_MIN);
        assert!((4u32, 19u32) < SEND_ZC_KERNEL_MIN);
    }

    #[test]
    fn classify_notification_cqe() {
        let mut transfer = None;
        let mut notif = false;
        classify_cqe(0, IORING_CQE_F_NOTIF, &mut transfer, &mut notif);
        assert!(notif);
        assert!(transfer.is_none());
    }

    #[test]
    fn classify_transfer_cqe_with_more_flag() {
        let mut transfer = None;
        let mut notif = false;
        classify_cqe(4096, IORING_CQE_F_MORE, &mut transfer, &mut notif);
        assert_eq!(transfer, Some(4096));
        assert!(!notif);
    }

    #[test]
    fn classify_transfer_cqe_error_without_notification() {
        // Kernel posts an error transfer CQE with `IORING_CQE_F_MORE`
        // cleared when the submission fails before any data is queued.
        let mut transfer = None;
        let mut notif = false;
        classify_cqe(-libc::EBADF, 0, &mut transfer, &mut notif);
        assert_eq!(transfer, Some(-libc::EBADF));
        assert!(notif, "missing F_MORE must synthesise the notification");
    }

    #[test]
    fn send_zc_rejects_empty_buffer() {
        // Empty-buffer rejection short-circuits before any io_uring call,
        // so a ring is built only to satisfy the borrowed-argument shape.
        let mut ring = match RawIoUring::new(4) {
            Ok(r) => r,
            Err(_) => return, // io_uring unavailable on this host
        };
        let err = try_send_zc(&mut ring, 0, &[], 0).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::InvalidInput);
    }

    /// Round-trips 64 KiB over a loopback TCP pair via `try_send_zc` and
    /// verifies both the byte content and that the notification CQE was
    /// observed (the call would not return otherwise).
    #[test]
    fn send_zc_roundtrip_64kib_loopback() {
        if !super::super::config::is_io_uring_available() {
            println!("skipping: io_uring unavailable on this host");
            return;
        }
        if !is_supported() {
            println!("skipping: IORING_OP_SEND_ZC unsupported on this kernel");
            return;
        }

        let listener = match TcpListener::bind("127.0.0.1:0") {
            Ok(l) => l,
            Err(e) => {
                println!("skipping: cannot bind loopback ({e})");
                return;
            }
        };
        let addr = listener.local_addr().unwrap();
        let reader_thread = thread::spawn(move || {
            let (mut peer, _) = listener.accept().unwrap();
            peer.set_read_timeout(Some(Duration::from_secs(10)))
                .unwrap();
            let mut received = Vec::with_capacity(64 * 1024);
            let mut tmp = [0u8; 8192];
            while received.len() < 64 * 1024 {
                let n = peer.read(&mut tmp).unwrap();
                if n == 0 {
                    break;
                }
                received.extend_from_slice(&tmp[..n]);
            }
            received
        });

        let sender = TcpStream::connect(addr).unwrap();
        let mut ring = IoUringConfig::default().build_ring().unwrap();
        let payload: Vec<u8> = (0..64 * 1024).map(|i| (i & 0xff) as u8).collect();

        let mut sent_total = 0usize;
        while sent_total < payload.len() {
            let n = try_send_zc(&mut ring, sender.as_raw_fd(), &payload[sent_total..], 0x42)
                .expect("SEND_ZC succeeds on loopback");
            assert!(n > 0, "kernel reported zero bytes sent");
            sent_total += n;
        }
        drop(sender);

        let received = reader_thread.join().unwrap();
        assert_eq!(received.len(), payload.len());
        assert_eq!(received, payload, "round-tripped bytes must match");
    }
}
