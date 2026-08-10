//! Test suite for the AIMD adaptive-concurrency limiter.
//!
//! Tests are split by topic into focused submodules:
//! - `basic`: acquire/release primitives, builder clamps, integer math.
//! - `aimd`: additive-increase / multiplicative-decrease semantics, slow-start
//!   transition, clamps, RTT smoothing, debounce.
//! - `convergence`: long-running convergence tests under steady patterns and
//!   error bursts.
//! - `concurrent`: multi-threaded contention and panic-path safety.

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
