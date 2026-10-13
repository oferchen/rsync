//! Thread-safe connection pool with per-IP connection counting.
//!
//! Tracks in-process connections and per-IP counts, complementing the
//! file-based `ConnectionLimiter` that enforces cross-process limits. Uses
//! `DashMap`, whose sharded locks let threads query and update connection
//! state concurrently.
//!
//! Deviation from upstream: upstream has no per-IP limit; its only connection
//! cap is the per-module `max connections` lock-file scan.

#![allow(dead_code)] // REASON: async daemon path not yet wired to production; types used in tests

mod pool;
mod types;

#[cfg(test)]
mod tests;

#[allow(unused_imports)] // REASON: re-export for async daemon path; used in tests
pub use pool::ConnectionPool;
#[allow(unused_imports)] // REASON: re-export for async daemon path; used in tests
pub use types::{AggregateStats, ConnectionId, ConnectionInfo, IpStats};
