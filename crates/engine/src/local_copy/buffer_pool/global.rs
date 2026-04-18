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
}

/// Environment variable for overriding the buffer pool size (number of buffers).
///
/// When set to a valid positive integer, overrides the auto-detected
/// hardware parallelism value. Useful for tuning memory usage in
/// constrained environments or for benchmarking.
const ENV_BUFFER_POOL_SIZE: &str = "OC_RSYNC_BUFFER_POOL_SIZE";

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
        let max_buffers = std::env::var(ENV_BUFFER_POOL_SIZE)
            .ok()
            .and_then(|v| v.parse::<usize>().ok())
            .filter(|&n| n > 0)
            .unwrap_or(auto_detected);
        Self {
            max_buffers,
            buffer_size: super::super::COPY_BUFFER_SIZE,
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
/// }).expect("pool not yet initialized");
/// ```
pub fn init_global_buffer_pool(
    config: GlobalBufferPoolConfig,
) -> Result<(), GlobalBufferPoolConfig> {
    let pool = Arc::new(BufferPool::with_buffer_size(
        config.max_buffers,
        config.buffer_size,
    ));
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
    }

    #[test]
    fn config_custom_values() {
        let config = GlobalBufferPoolConfig {
            max_buffers: 32,
            buffer_size: 512 * 1024,
        };
        assert_eq!(config.max_buffers, 32);
        assert_eq!(config.buffer_size, 512 * 1024);
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

    /// RAII guard that sets an env var and restores it on drop.
    struct EnvGuard {
        key: String,
        original: Option<String>,
    }

    impl EnvGuard {
        #[allow(unsafe_code)]
        fn set(key: &str, value: &str) -> Self {
            let original = std::env::var(key).ok();
            // SAFETY: tests using EnvGuard are serialized via nextest
            // test-group (max-threads = 1) to prevent concurrent env mutation.
            unsafe { std::env::set_var(key, value) };
            Self {
                key: key.to_string(),
                original,
            }
        }

        #[allow(unsafe_code)]
        fn remove(key: &str) -> Self {
            let original = std::env::var(key).ok();
            // SAFETY: see set() above.
            unsafe { std::env::remove_var(key) };
            Self {
                key: key.to_string(),
                original,
            }
        }
    }

    impl Drop for EnvGuard {
        #[allow(unsafe_code)]
        fn drop(&mut self) {
            // SAFETY: see EnvGuard::set() above.
            match &self.original {
                Some(val) => unsafe { std::env::set_var(&self.key, val) },
                None => unsafe { std::env::remove_var(&self.key) },
            }
        }
    }

    #[test]
    fn env_var_overrides_pool_size() {
        let _guard = EnvGuard::set(super::ENV_BUFFER_POOL_SIZE, "42");
        let config = GlobalBufferPoolConfig::default();
        assert_eq!(config.max_buffers, 42);
    }

    #[test]
    fn env_var_zero_ignored() {
        let _guard = EnvGuard::set(super::ENV_BUFFER_POOL_SIZE, "0");
        let config = GlobalBufferPoolConfig::default();
        // Zero is invalid, should fall back to auto-detected.
        let expected = std::thread::available_parallelism()
            .map(std::num::NonZero::get)
            .unwrap_or(4);
        assert_eq!(config.max_buffers, expected);
    }

    #[test]
    fn env_var_non_numeric_ignored() {
        let _guard = EnvGuard::set(super::ENV_BUFFER_POOL_SIZE, "not_a_number");
        let config = GlobalBufferPoolConfig::default();
        let expected = std::thread::available_parallelism()
            .map(std::num::NonZero::get)
            .unwrap_or(4);
        assert_eq!(config.max_buffers, expected);
    }

    #[test]
    fn env_var_negative_ignored() {
        let _guard = EnvGuard::set(super::ENV_BUFFER_POOL_SIZE, "-5");
        let config = GlobalBufferPoolConfig::default();
        let expected = std::thread::available_parallelism()
            .map(std::num::NonZero::get)
            .unwrap_or(4);
        assert_eq!(config.max_buffers, expected);
    }

    #[test]
    fn env_var_unset_uses_auto() {
        let _guard = EnvGuard::remove(super::ENV_BUFFER_POOL_SIZE);
        let config = GlobalBufferPoolConfig::default();
        let expected = std::thread::available_parallelism()
            .map(std::num::NonZero::get)
            .unwrap_or(4);
        assert_eq!(config.max_buffers, expected);
    }

    #[test]
    fn init_after_lazy_init_returns_err() {
        // Ensure the pool is initialized (may already be from other tests).
        let _pool = global_buffer_pool();

        let config = GlobalBufferPoolConfig {
            max_buffers: 99,
            buffer_size: 1024,
        };
        let result = init_global_buffer_pool(config);
        assert!(result.is_err());

        // The returned config should match what we passed in.
        let returned = result.unwrap_err();
        assert_eq!(returned.max_buffers, 99);
        assert_eq!(returned.buffer_size, 1024);
    }
}
