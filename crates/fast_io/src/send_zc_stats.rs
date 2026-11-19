//! Process-wide `IORING_OP_SEND_ZC` dispatch counters.
//!
//! The socket-send path degrades silently by design: a kernel without the
//! opcode, a blocked `io_uring_setup(2)` or a failed ring all fall back to a
//! plain send with byte-identical framing. These counters make that choice
//! observable, so tests and `--io-uring-status` can tell a SEND_ZC transfer
//! from one that only looked like it.
//!
//! The counts are per process. A forked daemon session child accumulates its
//! own and they are not visible to the parent.
//!
//! oc-only: upstream rsync has no io_uring path and no SEND_ZC.

use std::sync::atomic::{AtomicU64, Ordering};

static SENDS: AtomicU64 = AtomicU64::new(0);
static BYTES: AtomicU64 = AtomicU64::new(0);
static FALLBACKS: AtomicU64 = AtomicU64::new(0);
static COPIED_SENDS: AtomicU64 = AtomicU64::new(0);
static COPIED_BYTES: AtomicU64 = AtomicU64::new(0);

/// Snapshot of the process-wide SEND_ZC counters, read by [`send_zc_stats`].
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SendZcStats {
    /// `IORING_OP_SEND_ZC` submissions whose transfer completion reported
    /// bytes queued for transmit.
    pub sends: u64,
    /// Total bytes those transfer completions reported.
    pub bytes: u64,
    /// Socket writers whose policy permitted SEND_ZC but which were built
    /// without it, or lost it mid-stream, and used a plain send instead.
    pub fallbacks: u64,
    /// `IORING_OP_SEND_ZC` submissions the kernel refused for lack of
    /// pinnable memory (`ENOMEM`, `ENOBUFS`) whose bytes were then sent with a
    /// copying `IORING_OP_SEND` instead.
    pub copied_sends: u64,
    /// Total bytes those copying sends reported.
    pub copied_bytes: u64,
}

/// Returns the process-wide SEND_ZC counters.
#[must_use]
pub fn send_zc_stats() -> SendZcStats {
    SendZcStats {
        sends: SENDS.load(Ordering::Relaxed),
        bytes: BYTES.load(Ordering::Relaxed),
        fallbacks: FALLBACKS.load(Ordering::Relaxed),
        copied_sends: COPIED_SENDS.load(Ordering::Relaxed),
        copied_bytes: COPIED_BYTES.load(Ordering::Relaxed),
    }
}

/// Records one SEND_ZC transfer completion that queued `bytes` bytes.
#[cfg(all(target_os = "linux", feature = "io_uring"))]
pub(crate) fn record_send(bytes: usize) {
    SENDS.fetch_add(1, Ordering::Relaxed);
    BYTES.fetch_add(bytes as u64, Ordering::Relaxed);
}

/// Records one copying send of `bytes` bytes that replaced a SEND_ZC the
/// kernel refused for lack of pinnable memory.
#[cfg(all(target_os = "linux", feature = "io_uring"))]
pub(crate) fn record_copied_send(bytes: usize) {
    COPIED_SENDS.fetch_add(1, Ordering::Relaxed);
    COPIED_BYTES.fetch_add(bytes as u64, Ordering::Relaxed);
}

/// Records one SEND_ZC-permitted writer that ended up on a plain send.
///
/// Unix only: the raw-fd socket writer factories that call it do not exist
/// on Windows.
#[cfg(unix)]
pub(crate) fn record_fallback() {
    FALLBACKS.fetch_add(1, Ordering::Relaxed);
}
