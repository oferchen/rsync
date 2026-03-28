//! EMA-based throughput tracker for dynamic buffer sizing.
//!
//! Tracks bytes-per-second throughput using an Exponential Moving Average (EMA)
//! to smooth out per-transfer noise. The tracker is thread-safe and designed
//! for the hot path - recording a transfer sample requires only atomic operations.
//!
//! # EMA Formula
//!
//! ```text
//! ema = alpha * sample + (1 - alpha) * ema_prev
//! ```
//!
//! During the warmup period (first `WARMUP_SAMPLES` observations), a simple
//! cumulative average is used instead to avoid bias from the initial zero state.

use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::time::Duration;

/// Default smoothing factor for the EMA (0.1 = slow adaptation).
///
/// Lower values produce a smoother estimate that is less sensitive to
/// individual outlier samples. Higher values track recent throughput more
/// closely at the cost of stability.
const DEFAULT_ALPHA: f64 = 0.1;

/// Number of samples before switching from simple average to EMA.
///
/// During warmup the EMA would be biased toward zero, so a cumulative
/// average provides a more accurate initial estimate.
const WARMUP_SAMPLES: u32 = 8;

/// Minimum sample duration to accept (1 microsecond).
///
/// Samples shorter than this are discarded to avoid division-by-near-zero
/// artifacts from timer resolution limits.
const MIN_SAMPLE_DURATION: Duration = Duration::from_micros(1);

/// Minimum buffer size returned by [`recommended_buffer_size`](ThroughputTracker::recommended_buffer_size) (4 KiB).
pub const MIN_BUFFER_SIZE: usize = 4 * 1024;

/// Maximum buffer size returned by [`recommended_buffer_size`](ThroughputTracker::recommended_buffer_size) (256 KiB).
pub const MAX_BUFFER_SIZE: usize = 256 * 1024;

/// Target duration of data each buffer should hold (10 ms).
///
/// This balances syscall overhead (fewer, larger reads) against memory
/// consumption (smaller buffers waste less when throughput drops).
const TARGET_BUFFER_DURATION_SECS: f64 = 0.01;

/// Encodes an `f64` throughput value into a `u64` for atomic storage.
///
/// Uses `f64::to_bits` which is a lossless, portable bit-cast.
fn encode_throughput(value: f64) -> u64 {
    value.to_bits()
}

/// Decodes a `u64` back into the `f64` throughput value.
fn decode_throughput(bits: u64) -> f64 {
    f64::from_bits(bits)
}

/// Thread-safe throughput tracker using Exponential Moving Average.
///
/// Records transfer samples (bytes transferred over a duration) and maintains
/// a smoothed throughput estimate in bytes per second. The tracker uses atomic
/// operations for all state mutations, making it safe to call `record_transfer`
/// from any thread without locking.
///
/// # Usage
///
/// ```
/// use std::time::Duration;
/// use engine::local_copy::buffer_pool::throughput::ThroughputTracker;
///
/// let tracker = ThroughputTracker::new();
/// tracker.record_transfer(1_000_000, Duration::from_millis(10));
/// assert!(tracker.throughput_bps() > 0.0);
/// ```
#[derive(Debug)]
pub struct ThroughputTracker {
    /// Current EMA throughput estimate, stored as `f64::to_bits()`.
    ema_bits: AtomicU64,
    /// Cumulative sum of throughput samples during warmup.
    warmup_sum_bits: AtomicU64,
    /// Number of samples recorded so far.
    sample_count: AtomicU32,
    /// EMA smoothing factor (0.0 .. 1.0).
    alpha: f64,
}

impl ThroughputTracker {
    /// Creates a new tracker with the default smoothing factor (0.1).
    #[must_use]
    pub fn new() -> Self {
        Self::with_alpha(DEFAULT_ALPHA)
    }

    /// Creates a new tracker with a custom smoothing factor.
    ///
    /// # Arguments
    ///
    /// * `alpha` - Smoothing factor in the range `(0.0, 1.0]`. Values closer
    ///   to 0 produce a smoother (slower-reacting) estimate. Values closer to
    ///   1 track recent throughput more closely.
    ///
    /// # Panics
    ///
    /// Panics if `alpha` is not in `(0.0, 1.0]`.
    #[must_use]
    pub fn with_alpha(alpha: f64) -> Self {
        assert!(
            alpha > 0.0 && alpha <= 1.0,
            "alpha must be in (0.0, 1.0], got {alpha}"
        );
        Self {
            ema_bits: AtomicU64::new(encode_throughput(0.0)),
            warmup_sum_bits: AtomicU64::new(encode_throughput(0.0)),
            sample_count: AtomicU32::new(0),
            alpha,
        }
    }

    /// Records a transfer sample.
    ///
    /// Computes the instantaneous throughput (bytes / duration) and folds it
    /// into the EMA. During the warmup period, a simple cumulative average is
    /// used instead.
    ///
    /// Samples with zero bytes or duration shorter than 1 microsecond are
    /// silently discarded to avoid polluting the estimate with timer-resolution
    /// artifacts.
    pub fn record_transfer(&self, bytes: usize, duration: Duration) {
        if bytes == 0 || duration < MIN_SAMPLE_DURATION {
            return;
        }

        let bps = bytes as f64 / duration.as_secs_f64();
        if !bps.is_finite() || bps <= 0.0 {
            return;
        }

        // Increment sample count first (relaxed is fine - we only need eventual
        // visibility for the warmup/ema branch decision).
        let prev_count = self.sample_count.fetch_add(1, Ordering::Relaxed);
        let new_count = prev_count + 1;

        if new_count <= WARMUP_SAMPLES {
            // Warmup phase: accumulate sum and derive simple average.
            // CAS loop to atomically add `bps` to the running sum.
            loop {
                let old_bits = self.warmup_sum_bits.load(Ordering::Relaxed);
                let old_sum = decode_throughput(old_bits);
                let new_sum = old_sum + bps;
                if self
                    .warmup_sum_bits
                    .compare_exchange_weak(
                        old_bits,
                        encode_throughput(new_sum),
                        Ordering::Relaxed,
                        Ordering::Relaxed,
                    )
                    .is_ok()
                {
                    break;
                }
            }

            // Store the simple average as the current estimate.
            let sum = decode_throughput(self.warmup_sum_bits.load(Ordering::Relaxed));
            let avg = sum / f64::from(new_count);
            self.ema_bits
                .store(encode_throughput(avg), Ordering::Release);
        } else {
            // EMA phase: fold the new sample into the running average.
            // CAS loop for lock-free read-modify-write on the EMA.
            loop {
                let old_bits = self.ema_bits.load(Ordering::Acquire);
                let old_ema = decode_throughput(old_bits);
                let new_ema = self.alpha * bps + (1.0 - self.alpha) * old_ema;
                if self
                    .ema_bits
                    .compare_exchange_weak(
                        old_bits,
                        encode_throughput(new_ema),
                        Ordering::Release,
                        Ordering::Relaxed,
                    )
                    .is_ok()
                {
                    break;
                }
            }
        }
    }

    /// Returns the current throughput estimate in bytes per second.
    ///
    /// Returns `0.0` if no valid samples have been recorded yet.
    #[must_use]
    pub fn throughput_bps(&self) -> f64 {
        decode_throughput(self.ema_bits.load(Ordering::Acquire))
    }

    /// Returns the number of samples recorded so far.
    #[must_use]
    pub fn sample_count(&self) -> u32 {
        self.sample_count.load(Ordering::Relaxed)
    }

    /// Returns whether the tracker is still in the warmup period.
    #[must_use]
    pub fn is_warming_up(&self) -> bool {
        self.sample_count.load(Ordering::Relaxed) < WARMUP_SAMPLES
    }

    /// Computes a recommended buffer size based on current throughput.
    ///
    /// Targets [`TARGET_BUFFER_DURATION_SECS`] worth of data per buffer,
    /// clamped between [`MIN_BUFFER_SIZE`] and `max_size`. The result is
    /// rounded up to the next power of two for optimal I/O alignment.
    ///
    /// Returns [`MIN_BUFFER_SIZE`] if no throughput data is available yet.
    ///
    /// # Arguments
    ///
    /// * `max_size` - Upper bound on the returned size. Clamped to at least
    ///   [`MIN_BUFFER_SIZE`] internally.
    #[must_use]
    pub fn recommended_buffer_size(&self, max_size: usize) -> usize {
        let effective_max = max_size.clamp(MIN_BUFFER_SIZE, MAX_BUFFER_SIZE);
        let bps = self.throughput_bps();

        if bps <= 0.0 {
            return MIN_BUFFER_SIZE;
        }

        let raw = (bps * TARGET_BUFFER_DURATION_SECS) as usize;
        if raw <= MIN_BUFFER_SIZE {
            return MIN_BUFFER_SIZE;
        }

        // Round up to next power of two for I/O alignment.
        let rounded = raw.next_power_of_two();
        rounded.clamp(MIN_BUFFER_SIZE, effective_max)
    }
}

impl Default for ThroughputTracker {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::thread;

    #[test]
    fn new_tracker_has_zero_throughput() {
        let tracker = ThroughputTracker::new();
        assert_eq!(tracker.throughput_bps(), 0.0);
        assert_eq!(tracker.sample_count(), 0);
        assert!(tracker.is_warming_up());
    }

    #[test]
    fn single_sample_records_exact_throughput() {
        let tracker = ThroughputTracker::new();
        // 1 MB in 1 second = 1 MB/s
        tracker.record_transfer(1_000_000, Duration::from_secs(1));
        let bps = tracker.throughput_bps();
        assert!(
            (bps - 1_000_000.0).abs() < 1.0,
            "expected ~1MB/s, got {bps}"
        );
        assert_eq!(tracker.sample_count(), 1);
    }

    #[test]
    fn warmup_uses_simple_average() {
        let tracker = ThroughputTracker::new();

        // Record 4 samples at different rates.
        // 100 KB/s, 200 KB/s, 300 KB/s, 400 KB/s
        for rate_kbps in [100, 200, 300, 400] {
            let bytes = rate_kbps * 1000;
            tracker.record_transfer(bytes, Duration::from_secs(1));
        }

        // Simple average: (100K + 200K + 300K + 400K) / 4 = 250 KB/s
        let bps = tracker.throughput_bps();
        assert!(
            (bps - 250_000.0).abs() < 100.0,
            "expected ~250KB/s, got {bps}"
        );
        assert!(tracker.is_warming_up());
    }

    #[test]
    fn transitions_from_warmup_to_ema() {
        let tracker = ThroughputTracker::with_alpha(0.5);

        // Fill warmup with 8 samples at 1 MB/s
        for _ in 0..WARMUP_SAMPLES {
            tracker.record_transfer(1_000_000, Duration::from_secs(1));
        }
        assert!(!tracker.is_warming_up());

        let bps_before = tracker.throughput_bps();
        assert!(
            (bps_before - 1_000_000.0).abs() < 100.0,
            "expected ~1MB/s after warmup, got {bps_before}"
        );

        // Record one sample at 2 MB/s with alpha=0.5.
        // EMA = 0.5 * 2M + 0.5 * 1M = 1.5M
        tracker.record_transfer(2_000_000, Duration::from_secs(1));
        let bps_after = tracker.throughput_bps();
        assert!(
            (bps_after - 1_500_000.0).abs() < 100.0,
            "expected ~1.5MB/s, got {bps_after}"
        );
    }

    #[test]
    fn ema_smoothing_with_low_alpha() {
        let tracker = ThroughputTracker::with_alpha(0.1);

        // Fill warmup at 1 MB/s
        for _ in 0..WARMUP_SAMPLES {
            tracker.record_transfer(1_000_000, Duration::from_secs(1));
        }

        // Single spike to 10 MB/s should only move estimate slightly.
        // EMA = 0.1 * 10M + 0.9 * 1M = 1.9M
        tracker.record_transfer(10_000_000, Duration::from_secs(1));
        let bps = tracker.throughput_bps();
        assert!(
            (bps - 1_900_000.0).abs() < 1000.0,
            "expected ~1.9MB/s, got {bps}"
        );
    }

    #[test]
    fn zero_bytes_ignored() {
        let tracker = ThroughputTracker::new();
        tracker.record_transfer(0, Duration::from_secs(1));
        assert_eq!(tracker.sample_count(), 0);
        assert_eq!(tracker.throughput_bps(), 0.0);
    }

    #[test]
    fn very_short_duration_ignored() {
        let tracker = ThroughputTracker::new();
        tracker.record_transfer(1000, Duration::from_nanos(100));
        assert_eq!(tracker.sample_count(), 0);
    }

    #[test]
    fn zero_duration_ignored() {
        let tracker = ThroughputTracker::new();
        tracker.record_transfer(1000, Duration::ZERO);
        assert_eq!(tracker.sample_count(), 0);
    }

    #[test]
    fn recommended_size_no_data_returns_min() {
        let tracker = ThroughputTracker::new();
        assert_eq!(
            tracker.recommended_buffer_size(MAX_BUFFER_SIZE),
            MIN_BUFFER_SIZE
        );
    }

    #[test]
    fn recommended_size_low_throughput() {
        let tracker = ThroughputTracker::new();
        // 100 KB/s -> target = 100K * 0.01 = 1 KB -> clamped to MIN (4 KB)
        tracker.record_transfer(100_000, Duration::from_secs(1));
        assert_eq!(
            tracker.recommended_buffer_size(MAX_BUFFER_SIZE),
            MIN_BUFFER_SIZE
        );
    }

    #[test]
    fn recommended_size_medium_throughput() {
        let tracker = ThroughputTracker::new();
        // 10 MB/s -> target = 10M * 0.01 = 100 KB -> next power of 2 = 128 KB
        tracker.record_transfer(10_000_000, Duration::from_secs(1));
        let size = tracker.recommended_buffer_size(MAX_BUFFER_SIZE);
        assert_eq!(size, 128 * 1024);
    }

    #[test]
    fn recommended_size_high_throughput_clamped() {
        let tracker = ThroughputTracker::new();
        // 1 GB/s -> target = 1G * 0.01 = 10 MB -> clamped to MAX (256 KB)
        tracker.record_transfer(1_000_000_000, Duration::from_secs(1));
        let size = tracker.recommended_buffer_size(MAX_BUFFER_SIZE);
        assert_eq!(size, MAX_BUFFER_SIZE);
    }

    #[test]
    fn recommended_size_custom_max() {
        let tracker = ThroughputTracker::new();
        // 100 MB/s -> target = 100M * 0.01 = 1 MB -> clamped to 64 KB
        tracker.record_transfer(100_000_000, Duration::from_secs(1));
        let size = tracker.recommended_buffer_size(64 * 1024);
        assert_eq!(size, 64 * 1024);
    }

    #[test]
    fn recommended_size_is_power_of_two() {
        let tracker = ThroughputTracker::new();
        // Various throughput levels should all produce power-of-two sizes.
        for &rate in &[500_000usize, 2_000_000, 8_000_000, 50_000_000, 200_000_000] {
            tracker.record_transfer(rate, Duration::from_secs(1));
            let size = tracker.recommended_buffer_size(MAX_BUFFER_SIZE);
            assert!(
                size.is_power_of_two(),
                "size {size} is not power of two at rate {rate}"
            );
        }
    }

    #[test]
    fn recommended_size_stays_within_bounds() {
        let tracker = ThroughputTracker::new();
        // Sweep across a wide range of throughputs.
        for exp in 0..30u32 {
            let rate = 1usize << exp;
            let t = ThroughputTracker::new();
            t.record_transfer(rate, Duration::from_secs(1));
            let size = t.recommended_buffer_size(MAX_BUFFER_SIZE);
            assert!(size >= MIN_BUFFER_SIZE, "size {size} < MIN at rate {rate}");
            assert!(size <= MAX_BUFFER_SIZE, "size {size} > MAX at rate {rate}");
        }
        // Verify the unused tracker reference compiles.
        let _ = tracker;
    }

    #[test]
    fn concurrent_record_and_read() {
        let tracker = Arc::new(ThroughputTracker::new());
        let writer_count = 4;
        let reader_count = 4;
        let iterations = 500;

        let mut handles = Vec::new();

        // Writer threads record samples.
        for id in 0..writer_count {
            let t = Arc::clone(&tracker);
            handles.push(thread::spawn(move || {
                for i in 0..iterations {
                    let bytes = ((id + 1) * 100_000) + i * 1000;
                    t.record_transfer(bytes, Duration::from_millis(10));
                }
            }));
        }

        // Reader threads observe throughput.
        for _ in 0..reader_count {
            let t = Arc::clone(&tracker);
            handles.push(thread::spawn(move || {
                for _ in 0..iterations {
                    let bps = t.throughput_bps();
                    // Throughput should always be non-negative.
                    assert!(bps >= 0.0, "negative throughput: {bps}");
                }
            }));
        }

        for h in handles {
            h.join().expect("thread panicked");
        }

        // After all writers finish, throughput should be positive.
        assert!(tracker.throughput_bps() > 0.0);
        assert_eq!(
            tracker.sample_count(),
            writer_count as u32 * iterations as u32
        );
    }

    #[test]
    fn buffer_sizes_adapt_over_time() {
        let tracker = ThroughputTracker::with_alpha(0.5);

        // Start with low throughput - small buffers.
        for _ in 0..WARMUP_SAMPLES {
            tracker.record_transfer(100_000, Duration::from_secs(1));
        }
        let low_size = tracker.recommended_buffer_size(MAX_BUFFER_SIZE);

        // Ramp up throughput - buffers should grow.
        for _ in 0..20 {
            tracker.record_transfer(50_000_000, Duration::from_secs(1));
        }
        let high_size = tracker.recommended_buffer_size(MAX_BUFFER_SIZE);

        assert!(
            high_size > low_size,
            "buffer size did not grow: low={low_size}, high={high_size}"
        );
    }

    #[test]
    #[should_panic(expected = "alpha must be in (0.0, 1.0]")]
    fn alpha_zero_panics() {
        let _ = ThroughputTracker::with_alpha(0.0);
    }

    #[test]
    #[should_panic(expected = "alpha must be in (0.0, 1.0]")]
    fn alpha_negative_panics() {
        let _ = ThroughputTracker::with_alpha(-0.5);
    }

    #[test]
    #[should_panic(expected = "alpha must be in (0.0, 1.0]")]
    fn alpha_greater_than_one_panics() {
        let _ = ThroughputTracker::with_alpha(1.5);
    }

    #[test]
    fn alpha_one_is_valid() {
        let tracker = ThroughputTracker::with_alpha(1.0);
        // Alpha=1.0 means EMA tracks the latest sample exactly.
        for _ in 0..WARMUP_SAMPLES {
            tracker.record_transfer(1_000_000, Duration::from_secs(1));
        }
        tracker.record_transfer(5_000_000, Duration::from_secs(1));
        let bps = tracker.throughput_bps();
        assert!(
            (bps - 5_000_000.0).abs() < 100.0,
            "expected ~5MB/s with alpha=1.0, got {bps}"
        );
    }

    #[test]
    fn default_trait_works() {
        let tracker = ThroughputTracker::default();
        assert_eq!(tracker.sample_count(), 0);
        assert_eq!(tracker.throughput_bps(), 0.0);
    }

    #[test]
    fn subsecond_durations_produce_correct_rates() {
        let tracker = ThroughputTracker::new();
        // 1 KB in 1 ms = 1 MB/s
        tracker.record_transfer(1_000, Duration::from_millis(1));
        let bps = tracker.throughput_bps();
        assert!(
            (bps - 1_000_000.0).abs() < 10_000.0,
            "expected ~1MB/s, got {bps}"
        );
    }

    #[test]
    fn max_size_clamped_to_min() {
        let tracker = ThroughputTracker::new();
        // Even with max_size=0, should return MIN_BUFFER_SIZE.
        let size = tracker.recommended_buffer_size(0);
        assert_eq!(size, MIN_BUFFER_SIZE);
    }
}
