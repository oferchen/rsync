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

use std::io;
use std::os::unix::io::RawFd;
use std::sync::atomic::{AtomicI8, Ordering};

use io_uring::{IoUring as RawIoUring, opcode, types};

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
/// `count_supported_ops` cache in [`super::config`]. Distros backport features
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

/// One-shot probe: build a tiny ring, register a probe, check the opcode bit.
///
/// Returns `false` whenever the kernel rejects the probe entirely - the
/// caller treats both "kernel too old" and "probe blocked" as "unsupported"
/// because both surface as `io::ErrorKind::Unsupported` in the fall-back
/// path. Diagnostic separation lives in the existing
/// `config_detail::io_uring_kernel_info` reporter.
fn probe_send_zc() -> bool {
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
