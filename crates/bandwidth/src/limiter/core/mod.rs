//! Token-bucket bandwidth limiter split into focused submodules.
//!
//! - `limiter` - the `BandwidthLimiter` struct, construction, configuration, and rate enforcement
//! - `write_max` - chunk-size calculation from rate and burst parameters

mod limiter;
mod write_max;

pub use limiter::BandwidthLimiter;

#[cfg(test)]
mod tests;
