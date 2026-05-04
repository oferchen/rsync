//! Thread-safe connection pool with per-IP rate limiting.
//!
//! Provides connection tracking and rate limiting capabilities that complement
//! the file-based `ConnectionLimiter` for cross-process limits. Uses `DashMap`
//! for lock-free concurrent access, allowing multiple threads to query and
//! update connection state without blocking.

#![allow(dead_code)] // REASON: async daemon path not yet wired to production; types used in tests

mod pool;
mod types;

#[cfg(test)]
mod tests;

#[allow(unused_imports)] // REASON: re-export for async daemon path; used in tests
pub use pool::ConnectionPool;
#[allow(unused_imports)] // REASON: re-export for async daemon path; used in tests
pub use types::{AggregateStats, ConnectionId, ConnectionInfo, IpStats};
