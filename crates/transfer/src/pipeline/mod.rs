//! Request pipelining for rsync receiver transfer loop.
//!
//! This module implements pipelined file transfers to reduce latency overhead.
//! Instead of waiting for each file's delta response before requesting the next,
//! we send multiple requests ahead and process responses as they arrive.
//!
//! # Architecture Overview
//!
//! ```text
//! ┌─────────────────────────────────────────────────────────────────────┐
//! │                        Pipeline Architecture                        │
//! ├─────────────────────────────────────────────────────────────────────┤
//! │                                                                     │
//! │  Request Queue (bounded by window_size)                             │
//! │  ┌─────┬─────┬─────┬─────┬─────┐                                   │
//! │  │ R0  │ R1  │ R2  │ R3  │ ... │  → Send to sender                 │
//! │  └─────┴─────┴─────┴─────┴─────┘                                   │
//! │                                                                     │
//! │  Response Processing (in-order FIFO)                               │
//! │  ┌─────┬─────┬─────┬─────┬─────┐                                   │
//! │  │ D0  │ D1  │ D2  │ D3  │ ... │  ← Receive from sender            │
//! │  └─────┴─────┴─────┴─────┴─────┘                                   │
//! │                                                                     │
//! │  Outstanding Transfers (tracks in-flight requests)                 │
//! │  HashMap<ndx, PendingTransfer>                                     │
//! │                                                                     │
//! └─────────────────────────────────────────────────────────────────────┘
//! ```
//!
//! # Key Design Decisions
//!
//! 1. **In-order response processing**: The rsync protocol uses delta-encoded
//!    NDX values, requiring responses to be processed in the same order as
//!    requests. This simplifies synchronization.
//!
//! 2. **Bounded pipeline window**: Limits memory usage and prevents overwhelming
//!    the sender. Configurable via `--pipeline-window` CLI option.
//!
//! 3. **Signature generation during wait**: While waiting for responses, we can
//!    generate signatures for upcoming files, utilizing otherwise idle CPU time.
//!
//! # Performance Impact
//!
//! With 92,437 files and 0.5ms network latency per round-trip:
//! - Synchronous: 92,437 × 0.5ms = 46.2s latency overhead
//! - Pipelined (window=64): 92,437 / 64 × 0.5ms = 0.7s latency overhead
//!
//! This reduces latency overhead by ~65x.
//!
//! # Protocol Compatibility
//!
//! The pipelined receiver is fully compatible with upstream rsync daemons.
//! The sender doesn't need to know about pipelining - it simply processes
//! requests as they arrive and sends responses in order.

mod pending;
mod state;

pub use pending::PendingTransfer;
pub use state::PipelineState;

/// Default pipeline window size.
///
/// 64 concurrent requests provides good latency hiding without
/// excessive memory usage. Each pending transfer is ~500 bytes.
pub const DEFAULT_PIPELINE_WINDOW: usize = 64;

/// Minimum pipeline window size.
///
/// Must have at least 1 request in flight (synchronous mode).
pub const MIN_PIPELINE_WINDOW: usize = 1;

/// Maximum pipeline window size.
///
/// Limits memory usage. 256 requests × ~500 bytes = ~128KB.
pub const MAX_PIPELINE_WINDOW: usize = 256;

/// Configuration for pipelined transfers.
#[derive(Debug, Clone)]
pub struct PipelineConfig {
    /// Number of concurrent requests to keep in flight.
    pub window_size: usize,
    /// Whether to generate signatures asynchronously during pipeline waits.
    pub async_signatures: bool,
}

impl Default for PipelineConfig {
    fn default() -> Self {
        Self {
            window_size: DEFAULT_PIPELINE_WINDOW,
            async_signatures: true,
        }
    }
}

impl PipelineConfig {
    /// Creates a new pipeline configuration with the specified window size.
    #[must_use]
    pub fn with_window_size(mut self, window_size: usize) -> Self {
        self.window_size = window_size.clamp(MIN_PIPELINE_WINDOW, MAX_PIPELINE_WINDOW);
        self
    }

    /// Sets whether to generate signatures asynchronously.
    #[must_use]
    pub const fn with_async_signatures(mut self, enabled: bool) -> Self {
        self.async_signatures = enabled;
        self
    }

    /// Creates a synchronous configuration (window size = 1).
    #[must_use]
    pub fn synchronous() -> Self {
        Self {
            window_size: 1,
            async_signatures: false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_config_uses_default_window() {
        let config = PipelineConfig::default();
        assert_eq!(config.window_size, DEFAULT_PIPELINE_WINDOW);
        assert!(config.async_signatures);
    }

    #[test]
    fn with_window_size_clamps_to_min() {
        let config = PipelineConfig::default().with_window_size(0);
        assert_eq!(config.window_size, MIN_PIPELINE_WINDOW);
    }

    #[test]
    fn with_window_size_clamps_to_max() {
        let config = PipelineConfig::default().with_window_size(1000);
        assert_eq!(config.window_size, MAX_PIPELINE_WINDOW);
    }

    #[test]
    fn synchronous_config() {
        let config = PipelineConfig::synchronous();
        assert_eq!(config.window_size, 1);
        assert!(!config.async_signatures);
    }
}
