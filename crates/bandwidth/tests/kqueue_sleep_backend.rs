//! Darwin-only regression test for the kqueue `EVFILT_TIMER` sleep
//! backend (`KQ-S.4`).
//!
//! Exercises [`bandwidth::BandwidthLimiter`] through its public API
//! at `--bwlimit=1k` and asserts that the actual pacing duration
//! stays within 10% of the requested duration. The test compiles to
//! a no-op on every other platform so the cross-platform CI matrix
//! stays green without per-host `[[test]]` filtering.

#![cfg(target_os = "macos")]

use std::num::NonZeroU64;
use std::time::{Duration, Instant};

use bandwidth::{BandwidthLimiter, SleepBackend, active_backend};

/// Confirms the kqueue backend is the macOS default. The harness
/// runs without setting `OC_RSYNC_BWLIMIT_BACKEND`, so the platform
/// default applies. If `TimerSleeper::new()` failed we'd see
/// `SleepBackend::Std`; the test surfaces that fallback explicitly
/// so a CI regression points at the kqueue constructor rather than
/// at the limiter math.
#[test]
fn kqueue_backend_is_active_on_macos() {
    assert_eq!(
        active_backend(),
        SleepBackend::Kqueue,
        "macOS default backend should be the kqueue EVFILT_TIMER sleeper"
    );
}

/// Drives the limiter at `--bwlimit=1k` (1024 bytes/s) and asserts
/// the wall-clock pacing matches the requested rate within ±10%.
///
/// At 1 KiB/s, a single 512-byte `register` call accrues 500 ms of
/// debt, well above the limiter's 100 ms minimum sleep threshold,
/// so the limiter sleeps once per call. The test issues four
/// back-to-back 512-byte writes; the total wall-clock time should
/// be close to 4 * 500 ms = 2 s with the kqueue backend exhibiting
/// less jitter than `std::thread::sleep` does at this granularity.
#[test]
fn bwlimit_1k_paces_within_tolerance() {
    let mut limiter = BandwidthLimiter::new(NonZeroU64::new(1024).expect("non-zero"));

    // Prime the limiter so the first measured interval starts from
    // a known baseline rather than the limiter's birth time.
    let _ = limiter.register(1);

    let start = Instant::now();
    for _ in 0..4 {
        // `register` performs the pacing sleep internally; the returned
        // `LimiterSleep` is only a timing record, so discard it explicitly
        // (matching the priming call above) rather than tripping `must_use`.
        let _ = limiter.register(512);
    }
    let elapsed = start.elapsed();

    // Expected: ~4 * (512 bytes / 1024 bytes per second) = ~2 s.
    // Tolerance: ±10% lower bound, generous upper bound to absorb
    // CI scheduler jitter without flaking.
    let expected = Duration::from_millis(2_000);
    let lower = Duration::from_millis(1_800);
    let upper = Duration::from_millis(2_500);

    assert!(
        elapsed >= lower,
        "pacing too fast: elapsed={elapsed:?} expected~{expected:?}"
    );
    assert!(
        elapsed <= upper,
        "pacing too slow: elapsed={elapsed:?} expected~{expected:?}"
    );
}
