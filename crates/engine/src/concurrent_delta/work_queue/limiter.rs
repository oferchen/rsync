//! AIMD adaptive-concurrency limiter (task #2091).
//!
//! Implements the Additive-Increase / Multiplicative-Decrease control law
//! described in `docs/design/aimd-concurrency-limiter.md`. The limiter is
//! standalone in this PR and is not yet wired into [`WorkQueueSender`] or
//! the disk-commit side; integration arrives with #2092 (tests under
//! injected error) and #2093 (CLI flag).
//!
//! Design references:
//! - RFC 5681 section 3.1 (TCP congestion avoidance, AIMD law).
//! - RFC 6298 (RTT smoothing factor `alpha_ema = 1/8`).
//! - `docs/design/aimd-concurrency-limiter.md` sections 3.2-3.4.
//!
//! # Algorithm summary
//!
//! - Each acquired slot returns a [`Ticket`] that records its acquire time.
//! - `record_success` updates the RTT EMA and either doubles `target` (slow-start,
//!   while `last_decrease == 0`) or, after `target` consecutive successes, adds
//!   `alpha` (steady AIMD).
//! - `record_overload` halves `target` (clamped to `min_limit`) and resets the
//!   success counter, but only if the debounce window of `2 * rtt_ema` has
//!   elapsed since the last decrease.
//! - `record_error` classifies transient `io::ErrorKind`s as overload signals
//!   and treats the rest as successes (overload should reflect resource
//!   pressure, not deterministic filesystem state).
//!
//! All public types are wired into [`super`] via `pub mod limiter` and a
//! re-export. No callers consume them yet; integration tickets #2092 / #2093
//! will plug the limiter into [`WorkQueueSender::send`](super::WorkQueueSender)
//! and the CLI.
//!
//! [`WorkQueueSender`]: super::WorkQueueSender

use std::io;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicU32, AtomicU64, AtomicUsize, Ordering};
use std::time::Instant;

// RTT EMA smoothing factor: alpha_ema = RTT_EMA_NUM / RTT_EMA_DEN = 1/8
// per RFC 6298. RTT variance smoothing: 1/4 per RFC 6298.
const RTT_EMA_NUM: u64 = 1;
const RTT_EMA_DEN: u64 = 8;
const RTT_VAR_NUM: u64 = 1;
const RTT_VAR_DEN: u64 = 4;

/// Builder-pattern configuration for [`AimdLimiter`].
///
/// Construct with [`LimiterConfig::new`], then chain setters and call
/// [`LimiterConfig::build`] to produce an `AimdLimiter`. Sensible defaults
/// match the design RFC: alpha=1, beta=1/2, min_limit=rayon thread count,
/// max_limit=8x rayon thread count.
#[derive(Debug, Clone)]
#[must_use]
pub struct LimiterConfig {
    initial_target: usize,
    min_limit: usize,
    max_limit: usize,
    alpha: u32,
    beta_num: u32,
    beta_den: u32,
}

impl LimiterConfig {
    /// Returns a new config with sensible defaults for an `initial_target`.
    ///
    /// Defaults: `min_limit = 1`, `max_limit = 8 * initial_target.max(1)`,
    /// `alpha = 1`, `beta_num = 1`, `beta_den = 2` (multiplicative decrease 0.5).
    pub fn new(initial_target: usize) -> Self {
        let initial_target = initial_target.max(1);
        Self {
            initial_target,
            min_limit: 1,
            max_limit: initial_target.saturating_mul(8).max(initial_target),
            alpha: 1,
            beta_num: 1,
            beta_den: 2,
        }
    }

    /// Sets the floor `target` may decrease to. Clamped to at least 1.
    pub fn min_limit(mut self, min_limit: usize) -> Self {
        self.min_limit = min_limit.max(1);
        self
    }

    /// Sets the ceiling `target` may increase to.
    pub fn max_limit(mut self, max_limit: usize) -> Self {
        self.max_limit = max_limit.max(1);
        self
    }

    /// Sets the additive-increase step. Defaults to 1 (one slot per window).
    pub fn alpha(mut self, alpha: u32) -> Self {
        self.alpha = alpha.max(1);
        self
    }

    /// Sets the multiplicative-decrease ratio numerator.
    pub fn beta_num(mut self, beta_num: u32) -> Self {
        self.beta_num = beta_num.max(1);
        self
    }

    /// Sets the multiplicative-decrease ratio denominator. Must be > `beta_num`
    /// for the ratio to actually shrink `target`.
    pub fn beta_den(mut self, beta_den: u32) -> Self {
        self.beta_den = beta_den.max(2);
        self
    }

    /// Builds an [`AimdLimiter`] applying clamps so that `min_limit <= initial_target <= max_limit`.
    pub fn build(self) -> AimdLimiter {
        AimdLimiter::new(self)
    }
}

/// Reason a [`Ticket`] was released as an overload signal.
///
/// Maps to the four overload sources in design section 3.4.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OverloadReason {
    /// Completion latency exceeded `rtt_ema + 2 * sqrt(rtt_var)`.
    RttSpike,
    /// The bounded work queue rejected `try_send` with `Full`.
    QueueSaturated,
    /// The disk-commit writer reported high-water-mark pressure.
    DiskCommitPressure,
    /// The rolling error rate crossed the threshold (more than `target / 8`
    /// transient errors in the last `target` completions).
    ErrorRate,
}

/// AIMD adaptive-concurrency limiter.
///
/// See module-level docs for the algorithm. The limiter is `Send + Sync`;
/// callers may share it across rayon worker threads via `Arc`.
#[derive(Debug)]
pub struct AimdLimiter {
    target: AtomicUsize,
    in_flight: AtomicUsize,
    consecutive_successes: AtomicU32,
    rtt_ema: AtomicU64,
    rtt_var: AtomicU64,
    last_decrease: AtomicU64,
    min_limit: usize,
    max_limit: usize,
    alpha: u32,
    beta_num: u32,
    beta_den: u32,
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
        }
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
                    return Some(Ticket {
                        limiter: self,
                        acquired_at: Self::now_nanos(),
                        consumed: false,
                    });
                }
                Err(observed) => current = observed,
            }
        }
    }

    /// Releases a ticket as a success. Updates RTT EMA and may grow `target`.
    fn release_success(&self, acquired_at: u64) {
        let now = Self::now_nanos();
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
    fn release_overload(&self, acquired_at: u64, _reason: OverloadReason) {
        let now = Self::now_nanos();
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
    fn update_rtt(&self, sample: u64) {
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
    fn now_nanos() -> u64 {
        static EPOCH: OnceLock<Instant> = OnceLock::new();
        let epoch = EPOCH.get_or_init(Instant::now);
        epoch.elapsed().as_nanos() as u64
    }

    /// Integer square root via Newton's method. Used for the RTT-spike sigma
    /// threshold so the hot path avoids `f64::sqrt` and stays drift-free.
    fn isqrt(n: u64) -> u64 {
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

/// RAII slot guard returned by [`AimdLimiter::try_acquire`].
///
/// Callers must consume the ticket via one of [`Ticket::record_success`],
/// [`Ticket::record_overload`], or [`Ticket::record_error`]. Dropping the
/// ticket without recording (panic case) decrements `in_flight` without
/// touching `target` so the limiter stays consistent.
#[must_use = "the ticket reserves a slot; record success/overload/error to release it correctly"]
#[derive(Debug)]
pub struct Ticket<'a> {
    limiter: &'a AimdLimiter,
    acquired_at: u64,
    consumed: bool,
}

impl<'a> Ticket<'a> {
    /// Records a successful completion and releases the slot.
    pub fn record_success(mut self) {
        self.consumed = true;
        self.limiter.release_success(self.acquired_at);
    }

    /// Records an overload completion and releases the slot.
    pub fn record_overload(mut self, reason: OverloadReason) {
        self.consumed = true;
        self.limiter.release_overload(self.acquired_at, reason);
    }

    /// Records an `io::Error` completion. Transient kinds (`WouldBlock`,
    /// `Interrupted`, `TimedOut`) classify as [`OverloadReason::ErrorRate`];
    /// deterministic kinds (`NotFound`, `PermissionDenied`, etc.) are treated
    /// as success so that filesystem state does not collapse `target`.
    /// See design section 3.4 item (3).
    pub fn record_error(self, kind: io::ErrorKind) {
        if matches!(
            kind,
            io::ErrorKind::WouldBlock | io::ErrorKind::Interrupted | io::ErrorKind::TimedOut
        ) {
            self.record_overload(OverloadReason::ErrorRate);
        } else {
            self.record_success();
        }
    }
}

impl Drop for Ticket<'_> {
    fn drop(&mut self) {
        if !self.consumed {
            // Panic case: release the slot without disturbing target/EMA so the
            // limiter does not think we leaked a slot. We deliberately do NOT
            // call `release_success` here because we have no successful sample
            // to feed into the EMA.
            self.limiter.in_flight.fetch_sub(1, Ordering::AcqRel);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::panic;
    use std::sync::Arc;
    use std::sync::atomic::AtomicBool;
    use std::thread;
    use std::time::Duration;

    fn limiter_with(initial: usize, min: usize, max: usize) -> AimdLimiter {
        LimiterConfig::new(initial)
            .min_limit(min)
            .max_limit(max)
            .build()
    }

    #[test]
    fn isqrt_matches_floor_sqrt() {
        for n in [0u64, 1, 2, 3, 4, 9, 10, 100, 9999, 1_000_000, u64::MAX / 4] {
            let got = AimdLimiter::isqrt(n);
            assert!(got.saturating_mul(got) <= n, "isqrt({n}) = {got} too high");
            let next = got + 1;
            assert!(
                next.checked_mul(next).map(|sq| sq > n).unwrap_or(true),
                "isqrt({n}) = {got} too low"
            );
        }
    }

    #[test]
    fn acquire_respects_target() {
        let limiter = limiter_with(2, 1, 8);
        let t1 = limiter.try_acquire().expect("first slot");
        let t2 = limiter.try_acquire().expect("second slot");
        assert!(limiter.try_acquire().is_none(), "saturated");
        t1.record_success();
        assert!(limiter.try_acquire().is_some(), "freed by t1");
        t2.record_success();
    }

    #[test]
    fn acquire_saturation_under_threads() {
        // Stress the CAS loop: two threads racing to acquire the last slot.
        let limiter = Arc::new(limiter_with(1, 1, 4));
        let stop = Arc::new(AtomicBool::new(false));
        let stop_w = stop.clone();
        let limiter_w = limiter.clone();
        let handle = thread::spawn(move || {
            while !stop_w.load(Ordering::Acquire) {
                if let Some(t) = limiter_w.try_acquire() {
                    t.record_success();
                }
            }
        });
        thread::sleep(Duration::from_millis(20));
        for _ in 0..1000 {
            if let Some(t) = limiter.try_acquire() {
                t.record_success();
            }
        }
        stop.store(true, Ordering::Release);
        handle.join().unwrap();
        // The CAS loop guarantees in_flight returns to 0 after all tickets are
        // released. If the loop were broken, in_flight would be non-zero or
        // would have raced past target.
        assert_eq!(limiter.in_flight(), 0);
    }

    #[test]
    fn additive_increase_after_target_successes() {
        let limiter = limiter_with(2, 1, 16);
        // Force out of slow-start by recording one fake decrease.
        limiter
            .last_decrease
            .store(AimdLimiter::now_nanos(), Ordering::Release);
        // Two successes complete a window of size 2 -> target += 1.
        let t1 = limiter.try_acquire().unwrap();
        t1.record_success();
        let t2 = limiter.try_acquire().unwrap();
        t2.record_success();
        assert_eq!(limiter.target(), 3, "alpha=1 additive increase");
    }

    #[test]
    fn multiplicative_decrease_halves_target_on_overload() {
        let limiter = limiter_with(8, 1, 16);
        let t = limiter.try_acquire().unwrap();
        t.record_overload(OverloadReason::RttSpike);
        assert_eq!(limiter.target(), 4, "beta=1/2 multiplicative decrease");
    }

    #[test]
    fn decrease_clamped_to_min_limit() {
        let limiter = limiter_with(2, 2, 16);
        let t = limiter.try_acquire().unwrap();
        t.record_overload(OverloadReason::QueueSaturated);
        assert_eq!(limiter.target(), 2, "clamped at min_limit=2");
    }

    #[test]
    fn increase_clamped_to_max_limit() {
        let limiter = limiter_with(2, 1, 3);
        // Out of slow-start so additive applies.
        limiter
            .last_decrease
            .store(AimdLimiter::now_nanos(), Ordering::Release);
        // First window: target=2 -> 3 (clamped at max).
        let t1 = limiter.try_acquire().unwrap();
        t1.record_success();
        let t2 = limiter.try_acquire().unwrap();
        t2.record_success();
        assert_eq!(limiter.target(), 3);
        // Second window: still at 3 (clamped).
        for _ in 0..3 {
            let t = limiter.try_acquire().unwrap();
            t.record_success();
        }
        assert_eq!(limiter.target(), 3, "clamped at max_limit=3");
    }

    #[test]
    fn debounce_suppresses_back_to_back_decreases() {
        let limiter = limiter_with(8, 1, 16);
        // Seed a non-trivial RTT EMA so the debounce window has length.
        limiter.update_rtt(10_000_000); // 10 ms
        // First overload: decreases.
        let t1 = limiter.try_acquire().unwrap();
        t1.record_overload(OverloadReason::RttSpike);
        let after_first = limiter.target();
        assert!(after_first < 8);
        // Second overload immediately after: suppressed.
        let t2 = limiter.try_acquire().unwrap();
        t2.record_overload(OverloadReason::RttSpike);
        assert_eq!(
            limiter.target(),
            after_first,
            "debounce suppresses second decrease"
        );
    }

    #[test]
    fn rtt_ema_smoothing_converges() {
        let limiter = limiter_with(4, 1, 16);
        // Seed with a 1ms baseline, then drive 20 samples of 10ms. With
        // alpha_ema = 1/8 the geometric tail (7/8)^20 ~ 0.069, so EMA settles
        // between 9.0ms and 9.5ms after the second batch.
        for _ in 0..10 {
            limiter.update_rtt(1_000_000);
        }
        for _ in 0..20 {
            limiter.update_rtt(10_000_000);
        }
        let ema = limiter.rtt_ema_nanos();
        assert!(
            (9_000_000..=9_500_000).contains(&ema),
            "expected EMA in [9.0ms, 9.5ms], got {ema} ns",
        );
    }

    #[test]
    fn ticket_drop_on_panic_decrements_in_flight() {
        let limiter = Arc::new(limiter_with(2, 1, 8));
        let limiter_clone = limiter.clone();
        let result = panic::catch_unwind(panic::AssertUnwindSafe(move || {
            let _t = limiter_clone.try_acquire().expect("slot");
            assert_eq!(limiter_clone.in_flight(), 1);
            panic!("simulated worker panic");
        }));
        assert!(result.is_err(), "panic should propagate");
        assert_eq!(
            limiter.in_flight(),
            0,
            "Drop must decrement in_flight on unwind"
        );
        // Target untouched: drop path does not record success or overload.
        assert_eq!(limiter.target(), 2);
    }

    #[test]
    fn error_kind_classification() {
        let limiter = limiter_with(8, 1, 16);
        // Transient -> overload (decreases target).
        let t = limiter.try_acquire().unwrap();
        t.record_error(io::ErrorKind::WouldBlock);
        assert_eq!(limiter.target(), 4);

        // Reset for next case (debounce would otherwise suppress).
        let limiter = limiter_with(8, 1, 16);
        let t = limiter.try_acquire().unwrap();
        t.record_error(io::ErrorKind::Interrupted);
        assert_eq!(limiter.target(), 4);

        // Deterministic -> success path; target should not decrease.
        let limiter = limiter_with(8, 1, 16);
        let t = limiter.try_acquire().unwrap();
        t.record_error(io::ErrorKind::NotFound);
        assert_eq!(limiter.target(), 8);
        let t = limiter.try_acquire().unwrap();
        t.record_error(io::ErrorKind::PermissionDenied);
        assert_eq!(limiter.target(), 8);
    }

    #[test]
    fn slow_start_doubles_until_first_decrease() {
        let limiter = limiter_with(2, 1, 64);
        // Slow-start: complete a window of size 2 -> target doubles to 4.
        let t1 = limiter.try_acquire().unwrap();
        t1.record_success();
        let t2 = limiter.try_acquire().unwrap();
        t2.record_success();
        assert_eq!(limiter.target(), 4, "slow-start doubles 2 -> 4");

        // Complete another window of 4 -> target doubles to 8.
        for _ in 0..4 {
            let t = limiter.try_acquire().unwrap();
            t.record_success();
        }
        assert_eq!(limiter.target(), 8, "slow-start doubles 4 -> 8");

        // First overload exits slow-start and halves target.
        let t = limiter.try_acquire().unwrap();
        t.record_overload(OverloadReason::QueueSaturated);
        assert_eq!(limiter.target(), 4, "first decrease halves 8 -> 4");

        // Subsequent windows now grow additively (alpha=1) instead of doubling.
        // We must wait past the debounce window for further decreases, but
        // increases are unaffected. Need to wait long enough that the debounce
        // would have expired, but additive vs doubling is observable on the
        // success path regardless.
        std::thread::sleep(Duration::from_millis(5));
        // Saturate target=4 with successes: target -> 5 (additive, not 8).
        for _ in 0..4 {
            let t = limiter.try_acquire().unwrap();
            t.record_success();
        }
        assert_eq!(limiter.target(), 5, "post-decrease growth is additive");
    }

    #[test]
    fn rtt_spike_predicate() {
        let limiter = limiter_with(4, 1, 16);
        // No samples yet -> no spike.
        assert!(!limiter.is_rtt_spike(1_000_000_000));
        // Seed a stable EMA.
        for _ in 0..16 {
            limiter.update_rtt(1_000_000);
        }
        // A 100x spike is well above ema + 2*sigma.
        assert!(limiter.is_rtt_spike(100_000_000));
        // A sample at the EMA is not a spike.
        assert!(!limiter.is_rtt_spike(1_000_000));
    }

    #[test]
    fn config_builder_clamps_initial_target() {
        let limiter = LimiterConfig::new(100).min_limit(2).max_limit(10).build();
        assert_eq!(limiter.target(), 10, "initial clamped to max_limit");

        let limiter = LimiterConfig::new(1).min_limit(4).max_limit(16).build();
        assert_eq!(limiter.target(), 4, "initial clamped up to min_limit");
    }

    // --- Convergence tests (#2092) ---

    #[test]
    fn convergence_under_alternating_success_and_overload() {
        // Under a sustained pattern of success windows followed by single
        // overload signals, the target should stabilize within a bounded
        // range rather than diverging to min or max.
        let limiter = limiter_with(8, 2, 128);
        // Exit slow-start by injecting a first overload.
        let t = limiter.try_acquire().unwrap();
        t.record_overload(OverloadReason::RttSpike);
        let mut last_target = limiter.target();

        // Run 50 cycles of: one full success window, then one overload.
        // The debounce window starts near zero (fresh limiter), so we
        // sleep briefly between cycles to ensure decreases are not
        // suppressed.
        for _ in 0..50 {
            thread::sleep(Duration::from_millis(1));
            let window = limiter.target();
            for _ in 0..window {
                let t = limiter.try_acquire().unwrap();
                t.record_success();
            }
            let after_increase = limiter.target();

            thread::sleep(Duration::from_millis(1));
            let t = limiter.try_acquire().unwrap();
            t.record_overload(OverloadReason::QueueSaturated);
            last_target = limiter.target();

            // Additive increase (+1) followed by multiplicative decrease
            // (floor(n/2)) converges to a fixed point where
            // floor((n+1)/2) == n, which is n in {2, 3} for alpha=1, beta=1/2.
            // Allow a range [2, after_increase] to account for timing.
            assert!(
                last_target >= 2 && last_target <= after_increase,
                "target {last_target} should stay in [2, {after_increase}]",
            );
        }

        // After 50 cycles the target must have settled near the AIMD
        // equilibrium (which for alpha=1, beta=1/2 is 2 or 3).
        assert!(
            last_target <= 4,
            "expected convergence near 2-3, got {last_target}",
        );
    }

    #[test]
    fn sustained_successes_grow_target_monotonically() {
        let limiter = limiter_with(4, 1, 64);
        // Exit slow-start.
        let t = limiter.try_acquire().unwrap();
        t.record_overload(OverloadReason::DiskCommitPressure);

        thread::sleep(Duration::from_millis(1));
        let mut prev = limiter.target();
        // 10 consecutive success windows should produce monotonic growth.
        for _ in 0..10 {
            let window = limiter.target();
            for _ in 0..window {
                let t = limiter.try_acquire().unwrap();
                t.record_success();
            }
            let cur = limiter.target();
            assert!(
                cur >= prev,
                "target must not decrease during pure success: {prev} -> {cur}",
            );
            prev = cur;
        }
        assert!(
            prev > 4,
            "target should have grown beyond initial after 10 success windows, got {prev}",
        );
    }

    #[test]
    fn debounce_expires_after_twice_rtt_ema() {
        let limiter = limiter_with(16, 1, 64);
        // Seed RTT EMA to 5ms so debounce window = 10ms.
        for _ in 0..16 {
            limiter.update_rtt(5_000_000);
        }
        let t = limiter.try_acquire().unwrap();
        t.record_overload(OverloadReason::ErrorRate);
        let after_first = limiter.target();
        assert_eq!(after_first, 8);

        // Immediate second overload: debounce suppresses.
        let t = limiter.try_acquire().unwrap();
        t.record_overload(OverloadReason::ErrorRate);
        assert_eq!(limiter.target(), 8, "suppressed within debounce window");

        // Wait past 2 * rtt_ema (~10ms), then overload again.
        thread::sleep(Duration::from_millis(15));
        let t = limiter.try_acquire().unwrap();
        t.record_overload(OverloadReason::ErrorRate);
        assert_eq!(
            limiter.target(),
            4,
            "decrease should apply after debounce expires",
        );
    }

    #[test]
    fn slow_start_doubles_multiple_windows() {
        let limiter = limiter_with(1, 1, 256);
        let mut expected = 1;
        // During slow-start, each completed window doubles the target.
        for _ in 0..7 {
            let window = limiter.target();
            assert_eq!(window, expected);
            for _ in 0..window {
                let t = limiter.try_acquire().unwrap();
                t.record_success();
            }
            expected = expected.saturating_mul(2).min(256);
        }
        assert_eq!(
            limiter.target(),
            128,
            "slow-start should reach 128 after 7 doublings"
        );
    }

    #[test]
    fn overload_during_slow_start_transitions_to_additive() {
        let limiter = limiter_with(4, 1, 128);
        // One slow-start window: 4 -> 8.
        for _ in 0..4 {
            let t = limiter.try_acquire().unwrap();
            t.record_success();
        }
        assert_eq!(limiter.target(), 8);

        // Overload exits slow-start.
        let t = limiter.try_acquire().unwrap();
        t.record_overload(OverloadReason::RttSpike);
        assert_eq!(limiter.target(), 4);

        thread::sleep(Duration::from_millis(1));

        // Next window: additive, not doubling.
        for _ in 0..4 {
            let t = limiter.try_acquire().unwrap();
            t.record_success();
        }
        assert_eq!(
            limiter.target(),
            5,
            "post-overload growth must be additive (+1, not x2)"
        );

        for _ in 0..5 {
            let t = limiter.try_acquire().unwrap();
            t.record_success();
        }
        assert_eq!(limiter.target(), 6, "second additive window: 5 -> 6");
    }
}
