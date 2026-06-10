//! Per-file adaptive basis-read backend dispatch (SMR-3c, Option 3).
//!
//! EXPERIMENTAL. Gated by the `adaptive-basis-dispatch` Cargo feature, which
//! is **not** enabled by default. With the feature off, the module is not
//! compiled at all and the live dispatch path is byte-identical to today.
//!
//! # Background
//!
//! `docs/design/mmap-vs-sqpoll-conflict-resolution.md` enumerates three
//! resolution strategies for choosing between `mmap` and io_uring
//! `READ_FIXED` for basis-file reads. Option 2 (SMR-3b) wires a static
//! size threshold. This module implements Option 3: per-file dispatch
//! steered by rolling per-backend throughput statistics measured at run
//! time.
//!
//! # Algorithm
//!
//! For each completed basis read the caller invokes
//! [`crate::adaptive_dispatch::record_sample`] with the chosen backend, the byte count, and the
//! wall-clock duration. The module maintains an exponentially-weighted
//! moving average of bytes-per-second per backend with `alpha = 0.2`
//! per sample (the new sample contributes 20%, the running estimate
//! contributes 80%). [`crate::adaptive_dispatch::pick`] then compares the two EWMAs and returns
//! the faster backend, or falls back to the static size-threshold
//! heuristic from Option 2 ([`crate::adaptive_dispatch::size_threshold_pick`]) when one or both
//! backends has no recorded history.
//!
//! # Operator opt-out
//!
//! Setting `OC_RSYNC_ADAPTIVE_BASIS_DISPATCH=0` (or `off`, `false`,
//! `no`) in the environment disables the adaptive path at runtime even
//! when the feature is compiled in. [`crate::adaptive_dispatch::pick`] then always falls back to
//! the size-threshold path so the operator can revert to the static
//! Option 2 behaviour without rebuilding.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

/// Basis-file read backends considered by the adaptive dispatcher.
///
/// Mirrors the two arms produced by Option 2's size-threshold rule. A
/// third "buffered fallback" arm is not tracked: the adaptive dispatch
/// path only runs when one of these two backends is the candidate.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BasisReadBackend {
    /// File-backed `mmap` region. Best for warm pages and random
    /// access patterns; the page cache amortises sequential reads
    /// across multiple consumers (e.g. parallel checksum digest).
    Mmap,
    /// io_uring `IORING_OP_READ_FIXED` against a registered buffer.
    /// Best for SQPOLL-enabled rings on large basis files where the
    /// per-batch `io_uring_enter(2)` syscall otherwise dominates.
    IoUring,
}

/// Default size threshold for the Option 2 fallback rule, in bytes.
///
/// Files at or above this size prefer io_uring; smaller files prefer
/// mmap. The value mirrors the working-document recommendation in
/// `docs/design/mmap-vs-sqpoll-conflict-resolution.md`; it is tunable
/// per host via the SMR-3b wiring once that ships.
pub const DEFAULT_SIZE_THRESHOLD_BYTES: u64 = 16 * 1024 * 1024;

/// Smoothing factor for the per-backend EWMA. New samples weigh 20%;
/// the prior estimate weighs 80%. Bounded in `(0.0, 1.0]`.
const EWMA_ALPHA: f64 = 0.2;

/// Process-wide tracker that maintains an exponentially-weighted moving
/// average of throughput (bytes per second) for each basis-read backend.
///
/// The tracker is intentionally stateless across processes: SMR-3c is a
/// run-time heuristic, not a persisted statistic. Long-running daemons
/// accumulate enough samples within a single transfer for the EWMA to
/// stabilise; one-shot CLI invocations fall back to the size-threshold
/// rule on first dispatch and only diverge once a few files have been
/// transferred.
#[derive(Debug, Default)]
pub struct ThroughputTracker {
    /// Latest EWMA throughput (bytes/sec) for the mmap backend, stored
    /// as `f64::to_bits` inside an `AtomicU64` so reads of "have we got
    /// a sample yet?" are lock-free. Zero means "no samples recorded".
    mmap_ewma_bytes_per_sec: AtomicU64,
    /// Latest EWMA throughput (bytes/sec) for the io_uring backend.
    /// See [`Self::mmap_ewma_bytes_per_sec`] for the encoding.
    iouring_ewma_bytes_per_sec: AtomicU64,
    /// Wall-clock timestamp of the most recent sample. Held under a
    /// `Mutex` because `Instant` is not bit-portable; the mutex is
    /// only taken on the slow (record) path, never on the fast (pick)
    /// path.
    last_sample_ts: Mutex<Option<Instant>>,
}

impl ThroughputTracker {
    /// Construct a fresh tracker with no recorded samples on either
    /// backend.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Read the current EWMA for the requested backend. Returns
    /// `None` when no samples have been recorded.
    #[must_use]
    pub fn ewma_bytes_per_sec(&self, backend: BasisReadBackend) -> Option<f64> {
        let bits = match backend {
            BasisReadBackend::Mmap => self.mmap_ewma_bytes_per_sec.load(Ordering::Acquire),
            BasisReadBackend::IoUring => self.iouring_ewma_bytes_per_sec.load(Ordering::Acquire),
        };
        if bits == 0 {
            None
        } else {
            Some(f64::from_bits(bits))
        }
    }

    /// Fold a fresh throughput sample into the EWMA for `backend`.
    ///
    /// `bytes` is the payload size of the completed read and
    /// `elapsed` is the wall-clock duration the backend took to
    /// produce it. A zero-byte or zero-duration sample is dropped: it
    /// carries no throughput information and would either skew the
    /// EWMA toward zero or divide by zero.
    pub fn record_sample(&self, backend: BasisReadBackend, bytes: u64, elapsed: Duration) {
        if bytes == 0 {
            return;
        }
        let secs = elapsed.as_secs_f64();
        if secs <= 0.0 || !secs.is_finite() {
            return;
        }
        let sample = bytes as f64 / secs;
        if !sample.is_finite() || sample <= 0.0 {
            return;
        }

        let slot = match backend {
            BasisReadBackend::Mmap => &self.mmap_ewma_bytes_per_sec,
            BasisReadBackend::IoUring => &self.iouring_ewma_bytes_per_sec,
        };
        let prev_bits = slot.load(Ordering::Acquire);
        let updated = if prev_bits == 0 {
            sample
        } else {
            let prev = f64::from_bits(prev_bits);
            EWMA_ALPHA * sample + (1.0 - EWMA_ALPHA) * prev
        };
        slot.store(updated.to_bits(), Ordering::Release);

        if let Ok(mut guard) = self.last_sample_ts.lock() {
            *guard = Some(Instant::now());
        }
    }

    /// Wall-clock timestamp of the most recent recorded sample, or
    /// `None` if no samples have been folded in yet.
    #[must_use]
    pub fn last_sample_at(&self) -> Option<Instant> {
        self.last_sample_ts.lock().ok().and_then(|g| *g)
    }

    /// Reset both EWMAs and the last-sample timestamp. Used by tests
    /// to start from a known state; not exercised on the live path.
    #[cfg(test)]
    fn reset(&self) {
        self.mmap_ewma_bytes_per_sec.store(0, Ordering::Release);
        self.iouring_ewma_bytes_per_sec.store(0, Ordering::Release);
        if let Ok(mut guard) = self.last_sample_ts.lock() {
            *guard = None;
        }
    }
}

/// Process-wide singleton tracker. Lazily initialised on first call to
/// [`global_tracker`].
static GLOBAL_TRACKER: OnceLock<ThroughputTracker> = OnceLock::new();

/// Return the process-wide [`ThroughputTracker`], constructing it on
/// first call. Callers wiring the adaptive path through
/// [`crate::adaptive_dispatch::pick`] and [`crate::adaptive_dispatch::record_sample`] use this implicitly; tests that
/// want isolation should construct their own [`ThroughputTracker`]
/// instead.
#[must_use]
pub fn global_tracker() -> &'static ThroughputTracker {
    GLOBAL_TRACKER.get_or_init(ThroughputTracker::default)
}

/// Fold a fresh sample into the process-wide tracker. Convenience
/// wrapper around [`ThroughputTracker::record_sample`] on the
/// singleton from [`global_tracker`].
pub fn record_sample(backend: BasisReadBackend, bytes: u64, elapsed: Duration) {
    global_tracker().record_sample(backend, bytes, elapsed);
}

/// Pick a backend for the next basis-file read.
///
/// The order of decisions is:
///
/// 1. If the adaptive path is disabled at runtime
///    (`OC_RSYNC_ADAPTIVE_BASIS_DISPATCH=0`), fall straight back to
///    [`crate::adaptive_dispatch::size_threshold_pick`] (Option 2 behaviour).
/// 2. Filter out backends the caller declared unavailable. If only
///    one is available, return it.
/// 3. If both backends have recorded samples, return whichever has
///    the higher EWMA throughput. Ties resolve in favour of mmap
///    (the historically stable choice).
/// 4. Otherwise fall back to [`crate::adaptive_dispatch::size_threshold_pick`].
#[must_use]
pub fn pick(size: u64, mmap_available: bool, iouring_available: bool) -> BasisReadBackend {
    pick_with_tracker(global_tracker(), size, mmap_available, iouring_available)
}

/// [`crate::adaptive_dispatch::pick`] against a caller-supplied tracker. Lets tests exercise the
/// decision logic without contending on the process-wide singleton.
#[must_use]
pub fn pick_with_tracker(
    tracker: &ThroughputTracker,
    size: u64,
    mmap_available: bool,
    iouring_available: bool,
) -> BasisReadBackend {
    if !adaptive_enabled() {
        return size_threshold_pick(size, mmap_available, iouring_available);
    }

    match (mmap_available, iouring_available) {
        (true, false) => return BasisReadBackend::Mmap,
        (false, true) => return BasisReadBackend::IoUring,
        (false, false) => return BasisReadBackend::Mmap,
        (true, true) => {}
    }

    match (
        tracker.ewma_bytes_per_sec(BasisReadBackend::Mmap),
        tracker.ewma_bytes_per_sec(BasisReadBackend::IoUring),
    ) {
        (Some(mmap), Some(uring)) => {
            if uring > mmap {
                BasisReadBackend::IoUring
            } else {
                BasisReadBackend::Mmap
            }
        }
        _ => size_threshold_pick(size, mmap_available, iouring_available),
    }
}

/// Static size-threshold rule used as the fallback when the adaptive
/// path has no history to act on. Mirrors the Option 2 (SMR-3b)
/// recommendation: files at or above [`DEFAULT_SIZE_THRESHOLD_BYTES`]
/// prefer io_uring; smaller files prefer mmap.
///
/// When SMR-3b ships its own `fast_io::policy::choose_basis_read_backend`
/// helper the adaptive path will delegate to it instead; this function
/// keeps the adaptive feature self-contained until that lands.
#[must_use]
pub fn size_threshold_pick(
    size: u64,
    mmap_available: bool,
    iouring_available: bool,
) -> BasisReadBackend {
    match (mmap_available, iouring_available) {
        (true, false) => BasisReadBackend::Mmap,
        (false, true) => BasisReadBackend::IoUring,
        (false, false) => BasisReadBackend::Mmap,
        (true, true) => {
            if size >= DEFAULT_SIZE_THRESHOLD_BYTES {
                BasisReadBackend::IoUring
            } else {
                BasisReadBackend::Mmap
            }
        }
    }
}

/// Environment variable that disables the adaptive dispatcher at
/// runtime. Set to `0`, `off`, `false`, or `no` (case-insensitive) to
/// fall back to the static size-threshold rule without rebuilding.
pub const DISABLE_ENV_VAR: &str = "OC_RSYNC_ADAPTIVE_BASIS_DISPATCH";

fn adaptive_enabled() -> bool {
    match std::env::var(DISABLE_ENV_VAR) {
        Ok(value) => {
            let trimmed = value.trim();
            if trimmed == "0" {
                return false;
            }
            !trimmed.eq_ignore_ascii_case("off")
                && !trimmed.eq_ignore_ascii_case("false")
                && !trimmed.eq_ignore_ascii_case("no")
        }
        Err(_) => true,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex as StdMutex;

    /// Serialise tests that mutate the disable env var. The variable
    /// is process-global, so parallel tests would race; a dedicated
    /// guard keeps every assertion deterministic.
    static ENV_GUARD: StdMutex<()> = StdMutex::new(());

    struct EnvUnset;

    impl EnvUnset {
        fn new() -> Self {
            // SAFETY: env mutation is gated by `ENV_GUARD` above so no
            // concurrent test reads the variable while we clear it.
            unsafe {
                std::env::remove_var(DISABLE_ENV_VAR);
            }
            EnvUnset
        }
    }

    impl Drop for EnvUnset {
        fn drop(&mut self) {
            // SAFETY: see `EnvUnset::new`. Always restore an unset
            // state so unrelated tests inherit a clean env.
            unsafe {
                std::env::remove_var(DISABLE_ENV_VAR);
            }
        }
    }

    #[test]
    fn pick_uses_size_threshold_when_no_samples_recorded() {
        let _guard = ENV_GUARD.lock().unwrap();
        let _env = EnvUnset::new();
        let tracker = ThroughputTracker::new();

        // Small file with no history => mmap (size below threshold).
        assert_eq!(
            pick_with_tracker(&tracker, 4 * 1024, true, true),
            BasisReadBackend::Mmap
        );
        // Large file with no history => io_uring (size at threshold).
        assert_eq!(
            pick_with_tracker(&tracker, DEFAULT_SIZE_THRESHOLD_BYTES, true, true),
            BasisReadBackend::IoUring
        );
        // Only one backend available => that backend, regardless of
        // size or history.
        assert_eq!(
            pick_with_tracker(&tracker, DEFAULT_SIZE_THRESHOLD_BYTES, true, false),
            BasisReadBackend::Mmap
        );
        assert_eq!(
            pick_with_tracker(&tracker, 1, false, true),
            BasisReadBackend::IoUring
        );
    }

    #[test]
    fn pick_prefers_backend_with_higher_throughput() {
        let _guard = ENV_GUARD.lock().unwrap();
        let _env = EnvUnset::new();
        let tracker = ThroughputTracker::new();

        // Seed both backends: io_uring is twice as fast.
        tracker.record_sample(BasisReadBackend::Mmap, 1_000_000, Duration::from_millis(10));
        tracker.record_sample(
            BasisReadBackend::IoUring,
            2_000_000,
            Duration::from_millis(10),
        );

        // Size-threshold would pick mmap for the small file, but the
        // EWMA evidence dominates.
        assert_eq!(
            pick_with_tracker(&tracker, 4 * 1024, true, true),
            BasisReadBackend::IoUring
        );

        // Flip the evidence: mmap is now twice as fast as io_uring.
        tracker.reset();
        tracker.record_sample(
            BasisReadBackend::IoUring,
            1_000_000,
            Duration::from_millis(10),
        );
        tracker.record_sample(BasisReadBackend::Mmap, 2_000_000, Duration::from_millis(10));

        // Size-threshold would pick io_uring for the large file, but
        // the EWMA evidence dominates.
        assert_eq!(
            pick_with_tracker(&tracker, DEFAULT_SIZE_THRESHOLD_BYTES, true, true),
            BasisReadBackend::Mmap
        );
    }

    #[test]
    fn record_sample_updates_ewma() {
        let tracker = ThroughputTracker::new();

        // First sample seeds the EWMA directly.
        tracker.record_sample(BasisReadBackend::Mmap, 1_000_000, Duration::from_secs(1));
        let first = tracker
            .ewma_bytes_per_sec(BasisReadBackend::Mmap)
            .expect("first sample seeded");
        assert!((first - 1_000_000.0).abs() < 1e-6);

        // Second sample blends with the prior estimate using EWMA_ALPHA.
        tracker.record_sample(BasisReadBackend::Mmap, 2_000_000, Duration::from_secs(1));
        let second = tracker
            .ewma_bytes_per_sec(BasisReadBackend::Mmap)
            .expect("second sample folded in");
        let expected = EWMA_ALPHA * 2_000_000.0 + (1.0 - EWMA_ALPHA) * 1_000_000.0;
        assert!((second - expected).abs() < 1e-6);

        // Zero-byte and zero-duration samples are dropped.
        let before = second;
        tracker.record_sample(BasisReadBackend::Mmap, 0, Duration::from_secs(1));
        tracker.record_sample(BasisReadBackend::Mmap, 1_000_000, Duration::from_secs(0));
        let after = tracker
            .ewma_bytes_per_sec(BasisReadBackend::Mmap)
            .expect("still seeded");
        assert!((after - before).abs() < 1e-6);

        // The other backend is left untouched.
        assert!(
            tracker
                .ewma_bytes_per_sec(BasisReadBackend::IoUring)
                .is_none()
        );

        // Last-sample timestamp is populated.
        assert!(tracker.last_sample_at().is_some());
    }

    #[test]
    fn env_var_disables_adaptive_falls_back_to_threshold() {
        let _guard = ENV_GUARD.lock().unwrap();
        let _env = EnvUnset::new();
        let tracker = ThroughputTracker::new();

        // Seed evidence that would otherwise flip the decision.
        tracker.record_sample(BasisReadBackend::Mmap, 1, Duration::from_secs(1));
        tracker.record_sample(BasisReadBackend::IoUring, 1_000_000, Duration::from_secs(1));

        // Sanity-check: with adaptive on, the EWMA wins for a small file.
        assert_eq!(
            pick_with_tracker(&tracker, 4 * 1024, true, true),
            BasisReadBackend::IoUring
        );

        // SAFETY: env mutation is serialised by `ENV_GUARD` above.
        unsafe {
            std::env::set_var(DISABLE_ENV_VAR, "0");
        }
        assert_eq!(
            pick_with_tracker(&tracker, 4 * 1024, true, true),
            BasisReadBackend::Mmap,
            "adaptive disabled must fall back to size-threshold"
        );
        assert_eq!(
            pick_with_tracker(&tracker, DEFAULT_SIZE_THRESHOLD_BYTES, true, true),
            BasisReadBackend::IoUring,
            "size-threshold still routes large files to io_uring"
        );

        // SAFETY: env mutation is serialised by `ENV_GUARD` above.
        unsafe {
            std::env::set_var(DISABLE_ENV_VAR, "off");
        }
        assert_eq!(
            pick_with_tracker(&tracker, 4 * 1024, true, true),
            BasisReadBackend::Mmap
        );

        // Any other value re-enables the adaptive path.
        // SAFETY: env mutation is serialised by `ENV_GUARD` above.
        unsafe {
            std::env::set_var(DISABLE_ENV_VAR, "1");
        }
        assert_eq!(
            pick_with_tracker(&tracker, 4 * 1024, true, true),
            BasisReadBackend::IoUring
        );
    }
}
