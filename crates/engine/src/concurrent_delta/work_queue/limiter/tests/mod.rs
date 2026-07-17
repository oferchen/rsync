//! Test suite for the AIMD adaptive-concurrency limiter.
//!
//! Tests are split by topic into focused submodules:
//! - `basic`: acquire/release primitives, builder clamps, integer math.
//! - `aimd`: additive-increase / multiplicative-decrease semantics, slow-start
//!   transition, clamps, RTT smoothing, debounce.
//! - `convergence`: long-running convergence tests under steady patterns and
//!   error bursts.
//! - `concurrent`: multi-threaded contention and panic-path safety.

use std::cell::Cell;
use std::thread;
use std::time::Duration;

use super::{AimdLimiter, LimiterConfig, OverloadReason};

mod aimd;
mod basic;
mod concurrent;
mod convergence;

/// Builds a limiter with the requested initial/min/max settings.
fn limiter_with(initial: usize, min: usize, max: usize) -> AimdLimiter {
    LimiterConfig::new(initial)
        .min_limit(min)
        .max_limit(max)
        .build()
}

thread_local! {
    /// Virtual nanosecond clock for debounce/decay tests. Thread-local so each
    /// test (which runs on its own thread) drives an independent clock with no
    /// cross-test interference.
    static FAKE_CLOCK: Cell<u64> = const { Cell::new(0) };
}

/// Reads the thread-local virtual clock. Passed to [`AimdLimiter::with_clock`]
/// as a plain function pointer so the limiter's hot path stays allocation-free.
fn fake_now() -> u64 {
    FAKE_CLOCK.with(|c| c.get())
}

/// Handle that advances the virtual clock a limiter reads via [`fake_now`].
///
/// Debounce windows are measured against this clock instead of real elapsed
/// wall-clock time, so tests are immune to runner load: "within the window"
/// means the clock is not advanced, and "after the window" means the clock is
/// advanced explicitly past the debounce span.
struct TestClock;

impl TestClock {
    /// Advances the virtual clock by `nanos`.
    fn advance(&self, nanos: u64) {
        FAKE_CLOCK.with(|c| c.set(c.get().saturating_add(nanos)));
    }
}

/// Builds a limiter driven by the deterministic thread-local virtual clock,
/// returning the limiter and a handle to advance that clock.
fn limiter_with_clock(initial: usize, min: usize, max: usize) -> (AimdLimiter, TestClock) {
    FAKE_CLOCK.with(|c| c.set(1));
    let limiter = AimdLimiter::with_clock(
        LimiterConfig::new(initial).min_limit(min).max_limit(max),
        fake_now,
    );
    (limiter, TestClock)
}

/// Completes one full success window (target consecutive successes).
/// Returns the target after the window completes.
fn complete_success_window(limiter: &AimdLimiter) -> usize {
    let window = limiter.target();
    for _ in 0..window {
        let t = limiter.try_acquire().unwrap();
        t.record_success();
    }
    limiter.target()
}

/// Injects one overload signal, sleeping briefly to bypass debounce.
/// Returns the target after the overload is recorded.
fn inject_overload(limiter: &AimdLimiter, reason: OverloadReason) -> usize {
    thread::sleep(Duration::from_millis(1));
    let t = limiter.try_acquire().unwrap();
    t.record_overload(reason);
    limiter.target()
}
