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
//!
//! # ACK Batching
//!
//! When combined with the [`crate::ack_batcher`] module, acknowledgments for
//! completed transfers can be batched to further reduce network round-trips.
//! This is configured via [`PipelineConfig::ack_batch_size`].

mod pending;
mod state;
pub mod async_signature;

pub use pending::PendingTransfer;
pub use state::PipelineState;

use crate::ack_batcher::{
    AckBatcherConfig, DEFAULT_BATCH_SIZE, DEFAULT_BATCH_TIMEOUT_MS, MAX_BATCH_SIZE,
    MAX_BATCH_TIMEOUT_MS, MIN_BATCH_SIZE,
};

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
    /// Number of ACKs to batch before sending (0 = disabled).
    pub ack_batch_size: usize,
    /// Timeout in milliseconds before flushing ACK batch (0 = no timeout).
    pub ack_batch_timeout_ms: u64,
    /// Whether ACK batching is enabled.
    pub ack_batching_enabled: bool,
}

impl Default for PipelineConfig {
    fn default() -> Self {
        Self {
            window_size: DEFAULT_PIPELINE_WINDOW,
            async_signatures: true,
            ack_batch_size: DEFAULT_BATCH_SIZE,
            ack_batch_timeout_ms: DEFAULT_BATCH_TIMEOUT_MS,
            ack_batching_enabled: true,
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

    /// Sets the ACK batch size.
    ///
    /// # Arguments
    ///
    /// * `size` - Number of ACKs to batch (1-256). Values outside this range
    ///            are clamped.
    #[must_use]
    pub fn with_ack_batch_size(mut self, size: usize) -> Self {
        self.ack_batch_size = size.clamp(MIN_BATCH_SIZE, MAX_BATCH_SIZE);
        self
    }

    /// Sets the ACK batch timeout in milliseconds.
    ///
    /// # Arguments
    ///
    /// * `timeout_ms` - Maximum time to wait before flushing batch (max 1000ms).
    #[must_use]
    pub fn with_ack_batch_timeout_ms(mut self, timeout_ms: u64) -> Self {
        self.ack_batch_timeout_ms = timeout_ms.min(MAX_BATCH_TIMEOUT_MS);
        self
    }

    /// Enables or disables ACK batching.
    #[must_use]
    pub const fn with_ack_batching(mut self, enabled: bool) -> Self {
        self.ack_batching_enabled = enabled;
        self
    }

    /// Creates a synchronous configuration (window size = 1, no batching).
    #[must_use]
    pub fn synchronous() -> Self {
        Self {
            window_size: 1,
            async_signatures: false,
            ack_batch_size: 1,
            ack_batch_timeout_ms: 0,
            ack_batching_enabled: false,
        }
    }

    /// Creates an [`AckBatcherConfig`] from this pipeline configuration.
    #[must_use]
    pub fn ack_batcher_config(&self) -> AckBatcherConfig {
        if self.ack_batching_enabled {
            AckBatcherConfig::default()
                .with_batch_size(self.ack_batch_size)
                .with_timeout_ms(self.ack_batch_timeout_ms)
        } else {
            AckBatcherConfig::disabled()
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
        assert!(!config.ack_batching_enabled);
        assert_eq!(config.ack_batch_size, 1);
    }

    #[test]
    fn default_config_has_ack_batching() {
        let config = PipelineConfig::default();
        assert!(config.ack_batching_enabled);
        assert_eq!(config.ack_batch_size, DEFAULT_BATCH_SIZE);
        assert_eq!(config.ack_batch_timeout_ms, DEFAULT_BATCH_TIMEOUT_MS);
    }

    #[test]
    fn with_ack_batch_size_clamps() {
        let config = PipelineConfig::default().with_ack_batch_size(0);
        assert_eq!(config.ack_batch_size, MIN_BATCH_SIZE);

        let config = PipelineConfig::default().with_ack_batch_size(1000);
        assert_eq!(config.ack_batch_size, MAX_BATCH_SIZE);
    }

    #[test]
    fn with_ack_batch_timeout_clamps() {
        let config = PipelineConfig::default().with_ack_batch_timeout_ms(5000);
        assert_eq!(config.ack_batch_timeout_ms, MAX_BATCH_TIMEOUT_MS);
    }

    #[test]
    fn with_ack_batching_disabled() {
        let config = PipelineConfig::default().with_ack_batching(false);
        assert!(!config.ack_batching_enabled);
    }

    #[test]
    fn ack_batcher_config_from_pipeline_config() {
        let pipeline_config = PipelineConfig::default()
            .with_ack_batch_size(32)
            .with_ack_batch_timeout_ms(100);

        let ack_config = pipeline_config.ack_batcher_config();
        assert!(ack_config.is_enabled());
        assert_eq!(ack_config.batch_size, 32);
        assert_eq!(ack_config.batch_timeout_ms, 100);
    }

    #[test]
    fn ack_batcher_config_disabled() {
        let pipeline_config = PipelineConfig::default().with_ack_batching(false);
        let ack_config = pipeline_config.ack_batcher_config();
        assert!(!ack_config.is_enabled());
    }
}
