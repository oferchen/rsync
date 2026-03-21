//! Thread-safe connection pool with per-IP rate limiting.
//!
//! Provides connection tracking and rate limiting capabilities that complement
//! the file-based `ConnectionLimiter` for cross-process limits. Uses `DashMap`
//! for lock-free concurrent access, allowing multiple threads to query and
//! update connection state without blocking.

#![allow(dead_code)]

mod pool;
mod types;

#[cfg(test)]
mod tests;

pub use pool::ConnectionPool;
#[allow(unused_imports)]
pub use types::{AggregateStats, ConnectionId, ConnectionInfo, IpStats};
