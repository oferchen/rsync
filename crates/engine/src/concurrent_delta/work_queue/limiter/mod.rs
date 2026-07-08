//! AIMD adaptive-concurrency limiter.
//!
//! Implements the Additive-Increase / Multiplicative-Decrease control law
//! described in `docs/design/aimd-concurrency-limiter.md`. It backs the
//! [`AdaptiveQueueController`](super::controller::AdaptiveQueueController),
//! which drives the dynamic work-queue depth as internal autotuning - there is
//! no user-facing flag; the `OC_RSYNC_ADAPTIVE_QUEUE` env var only pins the
//! deterministic static depth for debugging.
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
//! All public types are re-exported from [`super`] via `pub mod limiter`.
//!
//! [`WorkQueueSender`]: super::WorkQueueSender

mod config;
mod rate;
mod ticket;

#[cfg(test)]
mod tests;

pub use config::LimiterConfig;
pub use rate::AimdLimiter;
pub use ticket::{OverloadReason, Ticket};
