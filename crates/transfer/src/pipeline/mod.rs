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
//!    the sender. Configurable via the `OC_RSYNC_PIPELINE_WINDOW` environment
//!    variable.
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

pub mod async_signature;
pub mod job;
pub mod messages;
mod pending;
pub mod receiver;
pub mod spsc;
mod state;

use std::env;

pub use job::{FileJob, FileList, MAX_RETRY_COUNT, TransferFlags};
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

/// Environment variable overriding the default pipeline window size.
///
/// Parsed as an unsigned integer and clamped to `[1, 256]`. Unset or
/// unparseable values fall back to [`DEFAULT_PIPELINE_WINDOW`]. Local
/// tuning knob only - never forwarded to a remote peer and never observable
/// on the wire.
pub const ENV_PIPELINE_WINDOW: &str = "OC_RSYNC_PIPELINE_WINDOW";

/// Resolves the default window size, honoring [`ENV_PIPELINE_WINDOW`].
///
/// Read once at config construction. Invalid values are ignored in favor
/// of [`DEFAULT_PIPELINE_WINDOW`], matching the tolerant parse style of
/// the other `OC_RSYNC_*` tuning knobs.
fn window_size_from_env() -> usize {
    env::var(ENV_PIPELINE_WINDOW)
        .ok()
        .and_then(|raw| raw.trim().parse::<u64>().ok())
        .map_or(DEFAULT_PIPELINE_WINDOW, |value| {
            usize::try_from(value)
                .unwrap_or(MAX_PIPELINE_WINDOW)
                .clamp(MIN_PIPELINE_WINDOW, MAX_PIPELINE_WINDOW)
        })
}

/// Configuration for pipelined transfers.
#[derive(Debug, Clone)]
pub struct PipelineConfig {
    /// Number of concurrent requests to keep in flight.
    pub window_size: usize,
}

impl Default for PipelineConfig {
    fn default() -> Self {
        Self {
            window_size: window_size_from_env(),
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

    /// Creates a synchronous configuration (window size = 1).
    #[must_use]
    pub fn synchronous() -> Self {
        Self { window_size: 1 }
    }
}

#[cfg(test)]
mod tests {
    use std::ffi::OsStr;
    use std::sync::Mutex;

    use platform::env::EnvGuard;

    use super::*;

    /// Serializes env mutation across this module's tests so concurrent
    /// test threads do not race on shared process state.
    static ENV_LOCK: Mutex<()> = Mutex::new(());

    #[test]
    fn default_config_uses_default_window() {
        let _lock = ENV_LOCK.lock().unwrap();
        let _guard = EnvGuard::remove(ENV_PIPELINE_WINDOW);
        let config = PipelineConfig::default();
        assert_eq!(config.window_size, DEFAULT_PIPELINE_WINDOW);
    }

    #[test]
    fn env_window_valid_value_applied() {
        let _lock = ENV_LOCK.lock().unwrap();
        let _guard = EnvGuard::set(ENV_PIPELINE_WINDOW, OsStr::new("32"));
        let config = PipelineConfig::default();
        assert_eq!(config.window_size, 32);
    }

    #[test]
    fn env_window_below_min_clamped() {
        let _lock = ENV_LOCK.lock().unwrap();
        let _guard = EnvGuard::set(ENV_PIPELINE_WINDOW, OsStr::new("0"));
        let config = PipelineConfig::default();
        assert_eq!(config.window_size, MIN_PIPELINE_WINDOW);
    }

    #[test]
    fn env_window_above_max_clamped() {
        let _lock = ENV_LOCK.lock().unwrap();
        let _guard = EnvGuard::set(ENV_PIPELINE_WINDOW, OsStr::new("100000"));
        let config = PipelineConfig::default();
        assert_eq!(config.window_size, MAX_PIPELINE_WINDOW);
    }

    #[test]
    fn env_window_garbage_falls_back_to_default() {
        let _lock = ENV_LOCK.lock().unwrap();
        let _guard = EnvGuard::set(ENV_PIPELINE_WINDOW, OsStr::new("not_a_number"));
        let config = PipelineConfig::default();
        assert_eq!(config.window_size, DEFAULT_PIPELINE_WINDOW);
    }

    #[test]
    fn env_window_negative_falls_back_to_default() {
        let _lock = ENV_LOCK.lock().unwrap();
        let _guard = EnvGuard::set(ENV_PIPELINE_WINDOW, OsStr::new("-4"));
        let config = PipelineConfig::default();
        assert_eq!(config.window_size, DEFAULT_PIPELINE_WINDOW);
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
    }

    #[test]
    fn with_window_size_accepts_valid_value() {
        let config = PipelineConfig::default().with_window_size(128);
        assert_eq!(config.window_size, 128);
    }

    #[test]
    fn builder_method_chaining() {
        let config = PipelineConfig::default().with_window_size(100);
        assert_eq!(config.window_size, 100);
    }
}
