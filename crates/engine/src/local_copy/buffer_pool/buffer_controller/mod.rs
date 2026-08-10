//! PID-style buffer-size controller.
//!
//! Implements the proportional-integral-derivative controller described in
//! `docs/design/adaptive-buffer-controller.md`. The controller lives in the
//! buffer pool subsystem alongside [`super::throughput`] which provides the
//! EMA-based throughput signal the controller consumes.
//!
//! The controller is wired into [`super::BufferPool`] via the
//! [`BufferPool::with_buffer_controller`](super::BufferPool::with_buffer_controller)
//! builder method. When enabled, [`BufferPool::record_transfer`](super::BufferPool::record_transfer)
//! feeds throughput samples to the controller, and
//! [`BufferPool::recommended_buffer_size`](super::BufferPool::recommended_buffer_size)
//! returns the controller's PID-driven recommendation instead of the
//! simpler EMA-based heuristic.
//!
//! Ziegler-Nichols closed-loop tuning is documented in section 6 of the
//! design doc; default gains shipped here come from that table.
//!
//! # Loop body
//!
//! ```text
//! e        = setpoint - throughput
//! P        = K_p * e
//! I       += K_i * e * dt    (clamped to anti-windup window)
//! D        = K_d * (e - e_prev) / dt
//! u_next   = clamp(u_prev + (P + I + D) * scale, min_size, max_size)
//! ```
//!
//! `scale` is `1.0` because the unit analysis works out: `e` is in bytes per
//! second, `dt` is in seconds, so `K_i * e * dt` is in bytes; `K_p * e` and
//! `K_d * (e - e_prev) / dt` are also in bytes once the dimensionless gains
//! are applied. The accumulator therefore moves the buffer size by an amount
//! whose units match the manipulated variable. See section 3.2 of the RFC.

mod config;
mod controller;
mod math;
mod state;

pub use config::ControllerConfig;
pub use controller::AdaptiveBufferController;

#[cfg(test)]
mod tests;
