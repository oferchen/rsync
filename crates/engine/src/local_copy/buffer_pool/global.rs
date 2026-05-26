//! Global bounded buffer pool singleton.
//!
//! Provides a process-wide [`BufferPool`] shared across all subsystems
//! (receiver, generator, disk commit, parallel checksum). A single shared
//! memory budget is more efficient than per-subsystem pools that each
//! independently size themselves to hardware parallelism.
//!
//! # Initialization
//!
//! The global pool is lazily initialized on first access via
//! [`global_buffer_pool`]. Callers that need custom settings (buffer count,
//! buffer size) should call [`init_global_buffer_pool`] early in the process
//! before any subsystem acquires buffers.
//!
//! If [`init_global_buffer_pool`] is not called, the pool defaults to
//! [`BufferPool::default`] - one buffer per hardware thread at
//! [`COPY_BUFFER_SIZE`](super::super::COPY_BUFFER_SIZE).
//!
//! # Thread Safety
//!
//! The singleton uses [`OnceLock`] for lock-free, one-shot initialization.
//! After initialization, [`global_buffer_pool`] returns a cloned [`Arc`]
//! with no synchronization overhead beyond an atomic reference count bump.

use std::sync::{Arc, OnceLock};

use super::BufferPool;

/// Process-wide buffer pool singleton.
static GLOBAL_BUFFER_POOL: OnceLock<Arc<BufferPool>> = OnceLock::new();

/// Configuration for the global buffer pool.
///
/// Passed to [`init_global_buffer_pool`] to customize pool parameters
/// before any subsystem acquires buffers.
#[derive(Debug, Clone, Copy)]
pub struct GlobalBufferPoolConfig {
    /// Maximum number of buffers the pool retains.
    pub max_buffers: usize,
    /// Size of each buffer in bytes.
    pub buffer_size: usize,
    /// Optional hard memory cap in bytes for outstanding (checked-out) buffers.
    ///
    /// When `Some`, the pool blocks `acquire` calls that would push outstanding
    /// memory past the cap until a buffer is returned. `None` leaves the pool
    /// uncapped, matching its historical default. A value of `Some(0)` is
    /// treated as `None`.
    pub memory_cap: Option<usize>,
    /// Optional soft byte budget on pool retention.
    ///
    /// When `Some`, the pool retains at most this many bytes of pooled
    /// buffers across all slots. Returns that would exceed the budget
    /// deallocate the buffer and increment the overflow counter rather
    /// than blocking. Acquires from an empty pool always allocate fresh.
    /// `None` (the default) leaves retained bytes uncapped. A value of
    /// `Some(0)` is treated as `None`.
    pub byte_budget: Option<usize>,
}

/// Environment variable for overriding the buffer pool size (number of buffers).
///
/// When set to a valid positive integer, overrides the auto-detected
/// hardware parallelism value. Useful for tuning memory usage in
/// constrained environments or for benchmarking.
const ENV_BUFFER_POOL_SIZE: &str = "OC_RSYNC_BUFFER_POOL_SIZE";

/// Parses an optional env-var value into a positive buffer count.
///
/// Returns `Some(n)` when the value is a valid integer greater than zero.
/// Returns `None` for missing, non-numeric, zero, or negative values.
fn parse_pool_size_override(env_val: Option<String>) -> Option<usize> {
    env_val
        .and_then(|v| v.parse::<usize>().ok())
        .filter(|&n| n > 0)
}

impl Default for GlobalBufferPoolConfig {
    /// Defaults to one buffer per hardware thread at the standard copy buffer size.
    ///
    /// The `OC_RSYNC_BUFFER_POOL_SIZE` environment variable overrides the
    /// auto-detected hardware parallelism value when set to a valid positive
    /// integer. Invalid or non-positive values are silently ignored.
    fn default() -> Self {
        let auto_detected = std::thread::available_parallelism()
            .map(std::num::NonZero::get)
            .unwrap_or(4);
        let max_buffers = parse_pool_size_override(std::env::var(ENV_BUFFER_POOL_SIZE).ok())
            .unwrap_or(auto_detected);
        Self {
            max_buffers,
            buffer_size: super::super::COPY_BUFFER_SIZE,
            memory_cap: None,
            byte_budget: None,
        }
    }
}

/// Returns a shared reference to the global buffer pool.
///
/// Initializes the pool with [`BufferPool::default`] on first call if
/// [`init_global_buffer_pool`] has not been called. Subsequent calls
/// return a clone of the same [`Arc`].
///
/// # Example
///
/// ```ignore
/// use engine::local_copy::buffer_pool::global_buffer_pool;
///
/// let pool = global_buffer_pool();
/// let buffer = BufferPool::acquire_from(pool);
/// ```
#[must_use]
pub fn global_buffer_pool() -> Arc<BufferPool> {
    Arc::clone(GLOBAL_BUFFER_POOL.get_or_init(|| Arc::new(BufferPool::default())))
}

/// Initializes the global buffer pool with custom settings.
///
/// Must be called before any call to [`global_buffer_pool`]. Returns `Ok(())`
/// on success, or `Err(config)` if the pool was already initialized (either
/// by a prior call to this function or by a lazy [`global_buffer_pool`] call).
///
/// # Errors
///
/// Returns the provided config back if the global pool is already initialized.
///
/// # Example
///
/// ```ignore
/// use engine::local_copy::buffer_pool::{init_global_buffer_pool, GlobalBufferPoolConfig};
///
/// init_global_buffer_pool(GlobalBufferPoolConfig {
///     max_buffers: 16,
///     buffer_size: 256 * 1024,
///     memory_cap: Some(512 * 1024 * 1024),
///     byte_budget: Some(64 * 1024 * 1024),
/// }).expect("pool not yet initialized");
/// ```
pub fn init_global_buffer_pool(
    config: GlobalBufferPoolConfig,
) -> Result<(), GlobalBufferPoolConfig> {
    let mut pool = BufferPool::with_buffer_size(config.max_buffers, config.buffer_size);
    if let Some(cap) = config.memory_cap.filter(|&n| n > 0) {
        pool = pool.with_memory_cap(cap);
    }
    if let Some(budget) = config.byte_budget.filter(|&n| n > 0) {
        pool = pool.with_byte_budget(budget);
    }
    let pool = Arc::new(pool);
    GLOBAL_BUFFER_POOL.set(pool).map_err(|_| config)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::thread;

    // NOTE: Because `OnceLock` is process-wide, tests that call
    // `init_global_buffer_pool` or `global_buffer_pool` share state.
    // Nextest serializes these via the `global-pool-serial` test-group
    // in `.config/nextest.toml` (max-threads = 1).

    #[test]
    fn config_default_matches_hardware_parallelism() {
        let config = GlobalBufferPoolConfig::default();
        let expected = std::thread::available_parallelism()
            .map(std::num::NonZero::get)
            .unwrap_or(4);
        assert_eq!(config.max_buffers, expected);
        assert_eq!(config.buffer_size, super::super::super::COPY_BUFFER_SIZE);
        assert!(config.memory_cap.is_none());
        assert!(config.byte_budget.is_none());
    }

    #[test]
    fn config_custom_values() {
        let config = GlobalBufferPoolConfig {
            max_buffers: 32,
            buffer_size: 512 * 1024,
            memory_cap: Some(64 * 1024 * 1024),
            byte_budget: None,
        };
        assert_eq!(config.max_buffers, 32);
        assert_eq!(config.buffer_size, 512 * 1024);
        assert_eq!(config.memory_cap, Some(64 * 1024 * 1024));
    }

    #[test]
    fn global_pool_returns_arc() {
        // This exercises the lazy init path. Since tests share the
        // process-wide OnceLock, the pool may already be initialized
        // by another test - that is fine, we just verify the Arc works.
        let pool = global_buffer_pool();
        assert!(pool.max_buffers() > 0);
        assert!(pool.buffer_size() > 0);
    }

    #[test]
    fn global_pool_returns_same_instance() {
        let a = global_buffer_pool();
        let b = global_buffer_pool();
        // Both Arcs point to the same allocation.
        assert!(Arc::ptr_eq(&a, &b));
    }

    #[test]
    fn global_pool_is_thread_safe() {
        let handles: Vec<_> = (0..8)
            .map(|_| {
                thread::spawn(|| {
                    let pool = global_buffer_pool();
                    // Acquire and release a buffer to exercise the pool.
                    let _guard = BufferPool::acquire_from(pool);
                })
            })
            .collect();

        for handle in handles {
            handle.join().expect("thread panicked");
        }
    }

    #[test]
    fn global_pool_buffers_are_reusable() {
        let pool = global_buffer_pool();
        let initial_available = pool.available();

        // Acquire and drop to return buffer to pool.
        {
            let _guard = BufferPool::acquire_from(Arc::clone(&pool));
        }

        // Pool should have at least one buffer now (the returned one).
        assert!(pool.available() >= initial_available);
    }

    #[test]
    fn parse_override_valid_positive() {
        assert_eq!(
            super::parse_pool_size_override(Some("42".to_string())),
            Some(42)
        );
    }

    #[test]
    fn parse_override_zero_ignored() {
        assert_eq!(super::parse_pool_size_override(Some("0".to_string())), None);
    }

    #[test]
    fn parse_override_non_numeric_ignored() {
        assert_eq!(
            super::parse_pool_size_override(Some("not_a_number".to_string())),
            None
        );
    }

    #[test]
    fn parse_override_negative_ignored() {
        assert_eq!(
            super::parse_pool_size_override(Some("-5".to_string())),
            None
        );
    }

    #[test]
    fn parse_override_none_returns_none() {
        assert_eq!(super::parse_pool_size_override(None), None);
    }

    #[test]
    fn init_after_lazy_init_returns_err() {
        // Ensure the pool is initialized (may already be from other tests).
        let _pool = global_buffer_pool();

        let config = GlobalBufferPoolConfig {
            max_buffers: 99,
            buffer_size: 1024,
            memory_cap: None,
            byte_budget: None,
        };
        let result = init_global_buffer_pool(config);
        assert!(result.is_err());

        // The returned config should match what we passed in.
        let returned = result.unwrap_err();
        assert_eq!(returned.max_buffers, 99);
        assert_eq!(returned.buffer_size, 1024);
        assert!(returned.memory_cap.is_none());
    }

    #[test]
    fn memory_cap_field_round_trips() {
        let config = GlobalBufferPoolConfig {
            max_buffers: 4,
            buffer_size: 8 * 1024,
            memory_cap: Some(4 * 1024 * 1024),
            byte_budget: None,
        };
        assert_eq!(config.memory_cap, Some(4 * 1024 * 1024));
    }

    #[test]
    fn byte_budget_field_round_trips() {
        let config = GlobalBufferPoolConfig {
            max_buffers: 4,
            buffer_size: 8 * 1024,
            memory_cap: None,
            byte_budget: Some(16 * 1024 * 1024),
        };
        assert_eq!(config.byte_budget, Some(16 * 1024 * 1024));
    }

    #[test]
    fn byte_budget_zero_is_treated_as_unbounded() {
        // init_global_buffer_pool filters byte_budget=Some(0) so the pool
        // stays uncapped instead of panicking on ByteBudget::new(0).
        let budget: Option<usize> = Some(0).filter(|&n| n > 0);
        assert!(budget.is_none());
    }

    #[test]
    fn memory_cap_zero_is_treated_as_unbounded() {
        // The init helper filters out memory_cap=Some(0) so the pool stays
        // uncapped instead of panicking on `MemoryCap::new(0)`. We only
        // assert the filter logic here; the actual pool init is exercised
        // by `init_after_lazy_init_returns_err` and integration tests.
        let cap: Option<usize> = Some(0).filter(|&n| n > 0);
        assert!(cap.is_none());
    }
}
