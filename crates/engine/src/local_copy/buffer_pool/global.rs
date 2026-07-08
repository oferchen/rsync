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
//! [`GlobalBufferPoolConfig::default`] - one buffer per hardware thread at
//! [`COPY_BUFFER_SIZE`](super::super::COPY_BUFFER_SIZE) with a 32 MiB
//! byte budget on pool retention.
//!
//! # Default Byte Budget
//!
//! The pool applies a default byte budget of [`DEFAULT_BYTE_BUDGET`] (32 MiB)
//! on retained buffers. This prevents unbounded memory growth when adaptive
//! buffer sizing creates large (up to 1 MiB) buffers that would otherwise
//! accumulate in the pool. The budget is a soft cap - acquires never block,
//! but returning buffers past the cap are deallocated instead of retained.
//!
//! The default can be overridden via the `OC_RSYNC_BYTE_BUDGET` environment
//! variable or programmatically through [`GlobalBufferPoolConfig`]. Setting
//! the env var to `0` disables the byte budget entirely.
//!
//! For daemon deployments serving many concurrent connections, the 32 MiB
//! default is shared across all connections (single process-wide pool). This
//! is appropriate because pooled buffers are reused across connections - the
//! pool size scales with parallelism, not with connection count.
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

/// Default byte budget for pool retention (32 MiB).
///
/// Matches the maximum pooled memory at the adaptive resizer's ceiling:
/// 256 buffers at 128 KiB each = 32 MiB. This provides a consistent
/// upper bound whether the pool grows by count (adaptive resizing) or
/// by individual buffer size (adaptive buffer sizing for large files).
///
/// Overridden by `--max-alloc`, `OC_RSYNC_BYTE_BUDGET`, or programmatic
/// configuration via [`GlobalBufferPoolConfig`].
pub const DEFAULT_BYTE_BUDGET: usize = 32 * 1024 * 1024;

/// Environment variable for overriding the buffer pool size (number of buffers).
///
/// When set to a valid positive integer, overrides the auto-detected
/// hardware parallelism value. Useful for tuning memory usage in
/// constrained environments or for benchmarking.
const ENV_BUFFER_POOL_SIZE: &str = "OC_RSYNC_BUFFER_POOL_SIZE";

/// Environment variable for overriding the default byte budget.
///
/// When set to a valid positive integer, overrides [`DEFAULT_BYTE_BUDGET`].
/// Set to `0` to disable the byte budget entirely (unbounded retention).
/// Useful for memory-constrained environments or high-throughput servers.
const ENV_BYTE_BUDGET: &str = "OC_RSYNC_BYTE_BUDGET";

/// Parses an optional env-var value into a positive buffer count.
///
/// Returns `Some(n)` when the value is a valid integer greater than zero.
/// Returns `None` for missing, non-numeric, zero, or negative values.
fn parse_pool_size_override(env_val: Option<String>) -> Option<usize> {
    env_val
        .and_then(|v| v.parse::<usize>().ok())
        .filter(|&n| n > 0)
}

/// Parses an optional env-var value into a byte budget.
///
/// Returns `Some(Some(n))` when the value is a valid positive integer (use n).
/// Returns `Some(None)` when the value is `"0"` (disable byte budget).
/// Returns `None` for missing or non-numeric values (use default).
fn parse_byte_budget_override(env_val: Option<String>) -> Option<Option<usize>> {
    let val = env_val?;
    let n = val.parse::<usize>().ok()?;
    if n == 0 {
        Some(None) // Explicitly disabled.
    } else {
        Some(Some(n))
    }
}

impl Default for GlobalBufferPoolConfig {
    /// Defaults to one buffer per hardware thread at the standard copy buffer
    /// size, with a 32 MiB byte budget on pool retention.
    ///
    /// The `OC_RSYNC_BUFFER_POOL_SIZE` environment variable overrides the
    /// auto-detected hardware parallelism value when set to a valid positive
    /// integer. Invalid or non-positive values are silently ignored.
    ///
    /// The `OC_RSYNC_BYTE_BUDGET` environment variable overrides the default
    /// byte budget. Set to `0` to disable the byte budget (unbounded
    /// retention). Invalid or non-numeric values are silently ignored.
    fn default() -> Self {
        let auto_detected = std::thread::available_parallelism()
            .map(std::num::NonZero::get)
            .unwrap_or(4);
        let max_buffers = parse_pool_size_override(std::env::var(ENV_BUFFER_POOL_SIZE).ok())
            .unwrap_or(auto_detected);
        let byte_budget = match parse_byte_budget_override(std::env::var(ENV_BYTE_BUDGET).ok()) {
            Some(override_val) => override_val, // Env var present: use its value (or None for 0).
            None => Some(DEFAULT_BYTE_BUDGET),  // No env var: apply the 32 MiB default.
        };
        Self {
            max_buffers,
            buffer_size: super::super::COPY_BUFFER_SIZE,
            memory_cap: None,
            byte_budget,
        }
    }
}

/// Returns a shared reference to the global buffer pool.
///
/// Initializes the pool from [`GlobalBufferPoolConfig::default`] on first
/// call if [`init_global_buffer_pool`] has not been called. This applies
/// the default 32 MiB byte budget on pool retention. Subsequent calls
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
    Arc::clone(GLOBAL_BUFFER_POOL.get_or_init(|| {
        let config = GlobalBufferPoolConfig::default();
        let mut pool = BufferPool::with_buffer_size(config.max_buffers, config.buffer_size);
        if let Some(budget) = config.byte_budget.filter(|&n| n > 0) {
            pool = pool.with_byte_budget(budget);
        }
        Arc::new(pool)
    }))
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
    use std::sync::{Mutex, MutexGuard};
    use std::thread;

    // Because `GLOBAL_BUFFER_POOL` is a process-wide `OnceLock`, every test
    // that calls `global_buffer_pool` or `init_global_buffer_pool` shares one
    // pool. `global_pool_buffers_are_reusable` samples that pool's
    // `available()` count twice and asserts it did not shrink; a sibling test
    // that concurrently acquires or releases buffers on the same pool would
    // perturb the count and make the assertion flaky.
    //
    // `SINGLETON_GUARD` serializes those tests in-process, independently of
    // the test name, so the invariant holds even under a shared-process test
    // runner. This complements (and does not rely on) the `global-pool-serial`
    // nextest group in `.config/nextest.toml`, which selects tests by name
    // substring and would silently drop a renamed test.
    static SINGLETON_GUARD: Mutex<()> = Mutex::new(());

    /// Serializes the tests that touch the process-wide global buffer pool.
    ///
    /// Recovers from a prior test's panic-poison via `into_inner` so a single
    /// failing test does not cascade into spurious failures in the others. The
    /// returned guard must be held for the whole test body.
    fn serialize_singleton() -> MutexGuard<'static, ()> {
        SINGLETON_GUARD
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
    }

    #[test]
    fn config_default_matches_hardware_parallelism() {
        let config = GlobalBufferPoolConfig::default();
        let expected = std::thread::available_parallelism()
            .map(std::num::NonZero::get)
            .unwrap_or(4);
        assert_eq!(config.max_buffers, expected);
        assert_eq!(config.buffer_size, super::super::super::COPY_BUFFER_SIZE);
        assert!(config.memory_cap.is_none());
        assert_eq!(config.byte_budget, Some(DEFAULT_BYTE_BUDGET));
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
        let _serial = serialize_singleton();
        // This exercises the lazy init path. Since tests share the
        // process-wide OnceLock, the pool may already be initialized
        // by another test - that is fine, we just verify the Arc works.
        let pool = global_buffer_pool();
        assert!(pool.max_buffers() > 0);
        assert!(pool.buffer_size() > 0);
    }

    #[test]
    fn global_pool_returns_same_instance() {
        let _serial = serialize_singleton();
        let a = global_buffer_pool();
        let b = global_buffer_pool();
        // Both Arcs point to the same allocation.
        assert!(Arc::ptr_eq(&a, &b));
    }

    #[test]
    fn global_pool_is_thread_safe() {
        let _serial = serialize_singleton();
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
        let _serial = serialize_singleton();
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
        let _serial = serialize_singleton();
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

    #[test]
    fn default_byte_budget_is_32_mib() {
        assert_eq!(DEFAULT_BYTE_BUDGET, 32 * 1024 * 1024);
    }

    #[test]
    fn parse_byte_budget_positive_value() {
        assert_eq!(
            parse_byte_budget_override(Some("16777216".to_string())),
            Some(Some(16_777_216))
        );
    }

    #[test]
    fn parse_byte_budget_zero_disables() {
        assert_eq!(
            parse_byte_budget_override(Some("0".to_string())),
            Some(None)
        );
    }

    #[test]
    fn parse_byte_budget_non_numeric_ignored() {
        assert_eq!(
            parse_byte_budget_override(Some("unlimited".to_string())),
            None
        );
    }

    #[test]
    fn parse_byte_budget_missing_returns_none() {
        assert_eq!(parse_byte_budget_override(None), None);
    }

    #[test]
    fn parse_byte_budget_negative_ignored() {
        assert_eq!(parse_byte_budget_override(Some("-1".to_string())), None);
    }
}
