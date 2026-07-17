//! AIMD rate-control core: target/in-flight bookkeeping plus the
//! RFC 6298 RTT EMA/variance math that drives the slow-start, additive
//! increase, and multiplicative decrease decisions.

use std::sync::OnceLock;
use std::sync::atomic::{AtomicU32, AtomicU64, AtomicUsize, Ordering};
use std::time::Instant;

use super::config::LimiterConfig;
use super::ticket::{OverloadReason, Ticket};

// RTT EMA smoothing factor: alpha_ema = RTT_EMA_NUM / RTT_EMA_DEN = 1/8
// per RFC 6298. RTT variance smoothing: 1/4 per RFC 6298.
const RTT_EMA_NUM: u64 = 1;
const RTT_EMA_DEN: u64 = 8;
const RTT_VAR_NUM: u64 = 1;
const RTT_VAR_DEN: u64 = 4;

/// AIMD adaptive-concurrency limiter.
///
/// See module-level docs for the algorithm. The limiter is `Send + Sync`;
/// callers may share it across rayon worker threads via `Arc`.
#[derive(Debug)]
pub struct AimdLimiter {
    target: AtomicUsize,
    pub(super) in_flight: AtomicUsize,
    consecutive_successes: AtomicU32,
    rtt_ema: AtomicU64,
    rtt_var: AtomicU64,
    pub(super) last_decrease: AtomicU64,
    min_limit: usize,
    max_limit: usize,
    alpha: u32,
    beta_num: u32,
    beta_den: u32,
    // Monotonic nanosecond clock. Production wires this to `now_nanos`; tests
    // may inject a deterministic clock so debounce windows are timed against a
    // controllable virtual clock rather than real wall-clock elapsed time.
    now: fn() -> u64,
}

impl AimdLimiter {
    /// Constructs a limiter from `config`. Prefer [`LimiterConfig::build`].
    pub fn new(config: LimiterConfig) -> Self {
        let LimiterConfig {
            initial_target,
            min_limit,
            max_limit,
            alpha,
            beta_num,
            beta_den,
        } = config;
        let max_limit = max_limit.max(min_limit);
        let initial_target = initial_target.clamp(min_limit, max_limit);
        Self {
            target: AtomicUsize::new(initial_target),
            in_flight: AtomicUsize::new(0),
            consecutive_successes: AtomicU32::new(0),
            rtt_ema: AtomicU64::new(0),
            rtt_var: AtomicU64::new(0),
            last_decrease: AtomicU64::new(0),
            min_limit,
            max_limit,
            alpha,
            beta_num,
            beta_den,
            now: Self::now_nanos,
        }
    }

    /// Constructs a limiter that reads time from `now` instead of the default
    /// monotonic clock. Test-only: lets debounce/decay behaviour be exercised
    /// against a deterministic virtual clock. Production always uses
    /// [`AimdLimiter::new`], which wires `now` to [`AimdLimiter::now_nanos`].
    #[cfg(test)]
    pub(super) fn with_clock(config: LimiterConfig, now: fn() -> u64) -> Self {
        let mut limiter = Self::new(config);
        limiter.now = now;
        limiter
    }

    /// Returns the current concurrency target.
    pub fn target(&self) -> usize {
        self.target.load(Ordering::Acquire)
    }

    /// Returns the current count of in-flight tickets.
    pub fn in_flight(&self) -> usize {
        self.in_flight.load(Ordering::Acquire)
    }

    /// Returns the smoothed RTT in nanoseconds. Zero before the first sample.
    pub fn rtt_ema_nanos(&self) -> u64 {
        self.rtt_ema.load(Ordering::Acquire)
    }

    /// Attempts to acquire a slot. Returns `None` when `in_flight >= target`.
    ///
    /// CAS-loops on `in_flight` so the count never races past `target`.
    pub fn try_acquire(&self) -> Option<Ticket<'_>> {
        let mut current = self.in_flight.load(Ordering::Acquire);
        loop {
            let target = self.target.load(Ordering::Acquire);
            if current >= target {
                return None;
            }
            match self.in_flight.compare_exchange_weak(
                current,
                current + 1,
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => {
                    return Some(Ticket::new(self, (self.now)()));
                }
                Err(observed) => current = observed,
            }
        }
    }

    /// Releases a ticket as a success. Updates RTT EMA and may grow `target`.
    pub(super) fn release_success(&self, acquired_at: u64) {
        let now = (self.now)();
        let sample = now.saturating_sub(acquired_at);
        self.update_rtt(sample);
        self.in_flight.fetch_sub(1, Ordering::AcqRel);

        // Slow-start: while we have not yet observed a decrease, double the
        // target on each completed window (one whole window of successes).
        let last_decrease = self.last_decrease.load(Ordering::Acquire);
        let target = self.target.load(Ordering::Acquire);
        let count = self.consecutive_successes.fetch_add(1, Ordering::AcqRel) + 1;

        if count >= target as u32 {
            self.consecutive_successes.store(0, Ordering::Release);
            let new_target = if last_decrease == 0 {
                target.saturating_mul(2)
            } else {
                target.saturating_add(self.alpha as usize)
            };
            let clamped = new_target.min(self.max_limit).max(self.min_limit);
            self.target.store(clamped, Ordering::Release);
        }
    }

    /// Releases a ticket as an overload signal. Halves `target` subject to
    /// the `2 * rtt_ema` debounce window.
    pub(super) fn release_overload(&self, acquired_at: u64, _reason: OverloadReason) {
        let now = (self.now)();
        // Still update the RTT EMA from this sample; the latency carries useful
        // information even on the overload path.
        let sample = now.saturating_sub(acquired_at);
        self.update_rtt(sample);
        self.in_flight.fetch_sub(1, Ordering::AcqRel);

        let last_decrease = self.last_decrease.load(Ordering::Acquire);
        let rtt_ema = self.rtt_ema.load(Ordering::Acquire);
        let debounce = rtt_ema.saturating_mul(2);

        // Suppress further decreases while inside the debounce window.
        if last_decrease != 0 && now.saturating_sub(last_decrease) < debounce {
            return;
        }

        let target = self.target.load(Ordering::Acquire);
        let new_target =
            (target as u64).saturating_mul(self.beta_num as u64) / self.beta_den as u64;
        let new_target = (new_target as usize).max(self.min_limit);
        self.target.store(new_target, Ordering::Release);
        self.consecutive_successes.store(0, Ordering::Release);
        self.last_decrease.store(now.max(1), Ordering::Release);
    }

    /// Updates the RTT EMA and variance. Uses RFC 6298 fixed-point math:
    /// `rtt_ema = (7 * rtt_ema + sample) / 8`,
    /// `rtt_var = (3 * rtt_var + |rtt_ema - sample|) / 4`.
    pub(super) fn update_rtt(&self, sample: u64) {
        if sample == 0 {
            return;
        }
        let prev_ema = self.rtt_ema.load(Ordering::Acquire);
        let new_ema = if prev_ema == 0 {
            sample
        } else {
            // (DEN-NUM)/DEN * prev + NUM/DEN * sample, computed as
            // ((DEN-NUM) * prev + NUM * sample) / DEN to avoid loss.
            let weight_prev = RTT_EMA_DEN - RTT_EMA_NUM;
            let weighted = prev_ema
                .saturating_mul(weight_prev)
                .saturating_add(sample.saturating_mul(RTT_EMA_NUM));
            weighted / RTT_EMA_DEN
        };
        self.rtt_ema.store(new_ema, Ordering::Release);

        let prev_var = self.rtt_var.load(Ordering::Acquire);
        let diff = sample.abs_diff(new_ema);
        let new_var = if prev_var == 0 {
            diff
        } else {
            let weight_prev = RTT_VAR_DEN - RTT_VAR_NUM;
            let weighted = prev_var
                .saturating_mul(weight_prev)
                .saturating_add(diff.saturating_mul(RTT_VAR_NUM));
            weighted / RTT_VAR_DEN
        };
        self.rtt_var.store(new_var, Ordering::Release);
    }

    /// Returns true if a sample is an RTT spike per design section 3.4.1.
    /// Threshold: `sample > rtt_ema + 2 * sqrt(rtt_var)`.
    pub fn is_rtt_spike(&self, sample_nanos: u64) -> bool {
        let ema = self.rtt_ema.load(Ordering::Acquire);
        if ema == 0 {
            return false;
        }
        let var = self.rtt_var.load(Ordering::Acquire);
        let sigma = Self::isqrt(var);
        let threshold = ema.saturating_add(sigma.saturating_mul(2));
        sample_nanos > threshold
    }

    /// Process-relative monotonic timestamp in nanoseconds. Uses a single
    /// [`OnceLock<Instant>`] epoch so all tickets share the same reference.
    pub(super) fn now_nanos() -> u64 {
        static EPOCH: OnceLock<Instant> = OnceLock::new();
        let epoch = EPOCH.get_or_init(Instant::now);
        epoch.elapsed().as_nanos() as u64
    }

    /// Integer square root via Newton's method. Used for the RTT-spike sigma
    /// threshold so the hot path avoids `f64::sqrt` and stays drift-free.
    pub(super) fn isqrt(n: u64) -> u64 {
        if n < 2 {
            return n;
        }
        let mut x = n;
        let mut y = x.div_ceil(2);
        while y < x {
            x = y;
            y = (x + n / x) / 2;
        }
        x
    }
}
