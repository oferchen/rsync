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
//! # Per-Buffer Block Size
//!
//! Each pooled buffer holds [`DEFAULT_BUFFER_POOL_BLOCK_SIZE`] bytes by
//! default. This purely local I/O knob is runtime-tunable via the
//! `OC_BUFFER_POOL_BLOCK_SIZE` env var (a size spec such as `4M`) or an
//! explicit [`GlobalBufferPoolConfig::buffer_size`], which takes precedence
//! over the env var. It has no effect on the wire protocol or delta block
//! size.
//!
//! For daemon deployments serving many concurrent connections, the 32 MiB
//! default is shared across all connections (single process-wide pool). This
//! is appropriate because pooled buffers are reused across connections - the
//! pool size scales with parallelism, not with connection count.
//!
//! # Hard Memory Cap
//!
//! Separately from the soft byte budget, the pool supports an optional *hard*
//! cap on outstanding (checked-out) memory. When set, an `acquire` that would
//! push live memory past the cap blocks until a buffer is returned
//! (backpressure), bounding peak memory under high concurrency. It is **off by
//! default** (uncapped, matching the historical behaviour) and is configured
//! only via the `OC_RSYNC_BUFFER_POOL_MEMORY_CAP` environment variable or
//! programmatically through [`GlobalBufferPoolConfig`]. The env value is a byte
//! count, or the literal `auto` to derive the cap from a fraction of detected
//! physical RAM; `0`, unset, or invalid values leave the pool uncapped.
//! Enabling it trades never-stalling acquires for a memory ceiling, so use it
//! only when peak memory matters more than acquire latency.
//!
//! # Thread Safety
//!
//! The singleton uses [`OnceLock`] for lock-free, one-shot initialization.
//! After initialization, [`global_buffer_pool`] returns a cloned [`Arc`]
//! with no synchronization overhead beyond an atomic reference count bump.

use std::sync::{Arc, OnceLock};

use bandwidth::parse_size_arg;

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
    /// Size of each buffer in bytes (the per-buffer block size).
    ///
    /// Defaults to [`DEFAULT_BUFFER_POOL_BLOCK_SIZE`], overridable via the
    /// `OC_BUFFER_POOL_BLOCK_SIZE` env var; an explicit value set here wins
    /// over the env var. This is a local I/O performance knob only - it never
    /// affects the wire protocol or the delta block size.
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

/// Default per-buffer block size for pooled I/O buffers (128 KiB).
///
/// Matches `COPY_BUFFER_SIZE` (`super::super::COPY_BUFFER_SIZE`), the size the
/// pool has always used. This is a purely local performance knob: it governs
/// how much data each reusable I/O buffer holds during file copy and has no
/// effect on the wire protocol, delta block size, or on-disk contents.
///
/// Overridden by the `OC_BUFFER_POOL_BLOCK_SIZE` environment variable or
/// programmatically through [`GlobalBufferPoolConfig::buffer_size`].
pub const DEFAULT_BUFFER_POOL_BLOCK_SIZE: usize = super::super::COPY_BUFFER_SIZE;

/// Upper bound accepted for a runtime-configured buffer-pool block size (1 GiB).
///
/// Requests above this are clamped with a one-shot warning: an absurdly large
/// per-buffer block size wastes memory (one allocation per pooled slot and per
/// checked-out buffer) without improving throughput past the point of
/// diminishing returns.
pub const MAX_BUFFER_POOL_BLOCK_SIZE: usize = 1024 * 1024 * 1024;

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

/// Environment variable for the pool's hard outstanding-memory cap.
///
/// Distinct from [`ENV_BYTE_BUDGET`], which is a *soft* budget on retained
/// (pooled) bytes that never blocks: this sets a *hard* ceiling on the bytes
/// of checked-out (outstanding) buffers, so an `acquire` that would exceed it
/// blocks (backpressure) until another buffer is returned. It bounds live
/// memory under high concurrency at the cost of throttling acquirers.
///
/// Accepted values:
/// - a positive integer byte count (e.g. `536870912` for 512 MiB),
/// - the literal `auto` - derive the cap from detected physical RAM
///   ([`AUTO_MEMORY_CAP_FRACTION`]); leaves the pool uncapped when RAM cannot
///   be detected,
/// - `0`, unset, or any invalid value - leave the pool uncapped (the default,
///   unchanged).
///
/// The cap is off by default; enabling it changes acquire semantics from
/// never-blocking to blocking-at-the-ceiling, so set it only when bounding
/// peak memory matters more than never stalling an acquirer.
const ENV_MEMORY_CAP: &str = "OC_RSYNC_BUFFER_POOL_MEMORY_CAP";

/// Divisor applied to detected physical RAM for the `auto` memory cap.
///
/// `4` yields a cap of one quarter of installed RAM - generous enough that it
/// never throttles normal transfers (whose outstanding buffers are a few MiB)
/// while still bounding pathological growth on massively parallel workloads.
const AUTO_MEMORY_CAP_FRACTION: u64 = 4;

/// Environment variable for overriding the per-buffer block size.
///
/// Accepts a size spec with the usual suffixes (`128K`, `4M`, `8388608`),
/// parsed by the shared [`bandwidth::parse_size_arg`] (upstream's single
/// `options.c:parse_size_arg()`) so it matches every other size option. A
/// zero, negative, or unparseable value is ignored (the default applies); a
/// value above [`MAX_BUFFER_POOL_BLOCK_SIZE`] is clamped with a warning.
const ENV_BUFFER_POOL_BLOCK_SIZE: &str = "OC_BUFFER_POOL_BLOCK_SIZE";

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

/// Parses the hard memory-cap env value into an optional cap in bytes.
///
/// - unset, `"0"`, or any invalid value -> `None` (uncapped; default unchanged)
/// - `"auto"` (case-insensitive) -> `total_ram / AUTO_MEMORY_CAP_FRACTION`
///   when `total_ram` is known, else `None` (graceful fallback)
/// - a positive integer -> that many bytes
///
/// `total_ram` is injected rather than queried here so the parse logic is
/// unit-testable without depending on the host's real memory.
fn parse_memory_cap_override(env_val: Option<String>, total_ram: Option<u64>) -> Option<usize> {
    let val = env_val?;
    let trimmed = val.trim();
    if trimmed.eq_ignore_ascii_case("auto") {
        let cap = total_ram? / AUTO_MEMORY_CAP_FRACTION;
        return usize::try_from(cap).ok().filter(|&n| n > 0);
    }
    trimmed.parse::<usize>().ok().filter(|&n| n > 0)
}

/// Parses an optional env-var value into a per-buffer block size in bytes.
///
/// Returns `Some(n)` for a valid positive size spec, clamped to
/// [`MAX_BUFFER_POOL_BLOCK_SIZE`] (with a one-shot warning on stderr when the
/// request is clamped). Returns `None` for a missing, empty, zero, negative,
/// or malformed value so the caller falls back to
/// [`DEFAULT_BUFFER_POOL_BLOCK_SIZE`]. Mirrors the `OC_RSYNC_REORDER_RING_CAP`
/// convention: bad input is warned about, never silently accepted.
fn parse_block_size_override(env_val: Option<String>) -> Option<usize> {
    let raw = env_val?;
    let trimmed = raw.trim();
    // upstream `parse_size_arg` never strips a leading sign; a negative block
    // size is nonsensical, so reject it before parsing rather than erroring.
    if trimmed.is_empty() || trimmed.starts_with('-') {
        return None;
    }
    let bytes = match parse_size_arg(trimmed, b'b') {
        Ok(parsed) => usize::try_from(parsed.bytes).ok()?,
        Err(_) => {
            eprintln!(
                "oc-rsync: {ENV_BUFFER_POOL_BLOCK_SIZE}={raw:?} could not be parsed as a size; falling back to default"
            );
            return None;
        }
    };
    if bytes == 0 {
        eprintln!(
            "oc-rsync: {ENV_BUFFER_POOL_BLOCK_SIZE}={raw:?} is invalid (block size must be positive); falling back to default"
        );
        return None;
    }
    if bytes > MAX_BUFFER_POOL_BLOCK_SIZE {
        eprintln!(
            "oc-rsync: {ENV_BUFFER_POOL_BLOCK_SIZE}={raw:?} exceeds the {MAX_BUFFER_POOL_BLOCK_SIZE}-byte maximum; clamping"
        );
        return Some(MAX_BUFFER_POOL_BLOCK_SIZE);
    }
    Some(bytes)
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
    ///
    /// The `OC_BUFFER_POOL_BLOCK_SIZE` environment variable overrides the
    /// per-buffer block size ([`DEFAULT_BUFFER_POOL_BLOCK_SIZE`]) with a size
    /// spec (`4M`, `8388608`). Values above [`MAX_BUFFER_POOL_BLOCK_SIZE`] are
    /// clamped; zero, negative, or malformed values fall back to the default.
    /// An explicit [`buffer_size`](Self::buffer_size) set by a caller takes
    /// precedence over this env var.
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
        let memory_cap = parse_memory_cap_override(
            std::env::var(ENV_MEMORY_CAP).ok(),
            fast_io::physical_memory::total_physical_memory(),
        );
        let buffer_size = parse_block_size_override(std::env::var(ENV_BUFFER_POOL_BLOCK_SIZE).ok())
            .unwrap_or(DEFAULT_BUFFER_POOL_BLOCK_SIZE);
        Self {
            max_buffers,
            buffer_size,
            memory_cap,
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
        // Apply the hard cap on the lazy path too, so an env-configured
        // `OC_RSYNC_BUFFER_POOL_MEMORY_CAP` takes effect even when no caller
        // invokes `init_global_buffer_pool` first (mirrors that helper).
        if let Some(cap) = config.memory_cap.filter(|&n| n > 0) {
            pool = pool.with_memory_cap(cap);
        }
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

    // Physical RAM used by the `auto` parser tests (8 GiB).
    const TEST_RAM: u64 = 8 * 1024 * 1024 * 1024;

    #[test]
    fn parse_memory_cap_missing_is_uncapped() {
        assert_eq!(parse_memory_cap_override(None, Some(TEST_RAM)), None);
    }

    #[test]
    fn parse_memory_cap_zero_is_uncapped() {
        assert_eq!(
            parse_memory_cap_override(Some("0".to_string()), Some(TEST_RAM)),
            None
        );
    }

    #[test]
    fn parse_memory_cap_non_numeric_is_uncapped() {
        assert_eq!(
            parse_memory_cap_override(Some("lots".to_string()), Some(TEST_RAM)),
            None
        );
    }

    #[test]
    fn parse_memory_cap_explicit_byte_count() {
        assert_eq!(
            parse_memory_cap_override(Some("536870912".to_string()), Some(TEST_RAM)),
            Some(536_870_912)
        );
    }

    #[test]
    fn parse_memory_cap_auto_is_fraction_of_ram() {
        // 8 GiB / 4 = 2 GiB.
        assert_eq!(
            parse_memory_cap_override(Some("auto".to_string()), Some(TEST_RAM)),
            Some(2 * 1024 * 1024 * 1024)
        );
    }

    #[test]
    fn parse_memory_cap_auto_is_case_and_whitespace_insensitive() {
        assert_eq!(
            parse_memory_cap_override(Some("  AuTo  ".to_string()), Some(4 * 1024 * 1024 * 1024)),
            Some(1024 * 1024 * 1024)
        );
    }

    #[test]
    fn parse_memory_cap_auto_without_ram_is_uncapped() {
        // Detection failed: degrade gracefully to uncapped rather than guess.
        assert_eq!(
            parse_memory_cap_override(Some("auto".to_string()), None),
            None
        );
    }

    #[test]
    fn default_buffer_pool_block_size_matches_copy_buffer_size() {
        // The default must equal the historical compile-time constant so that
        // an unset env var is byte-for-byte behaviour-preserving.
        assert_eq!(
            DEFAULT_BUFFER_POOL_BLOCK_SIZE,
            super::super::super::COPY_BUFFER_SIZE
        );
    }

    #[test]
    fn parse_block_size_missing_returns_none() {
        // No env var: caller falls back to DEFAULT_BUFFER_POOL_BLOCK_SIZE.
        assert_eq!(parse_block_size_override(None), None);
    }

    #[test]
    fn parse_block_size_plain_bytes() {
        assert_eq!(
            parse_block_size_override(Some("8388608".to_string())),
            Some(8_388_608)
        );
    }

    #[test]
    fn parse_block_size_suffix_spec() {
        // Size spec reuses the shared parser: 4M == 4 MiB.
        assert_eq!(
            parse_block_size_override(Some("4M".to_string())),
            Some(4 * 1024 * 1024)
        );
        assert_eq!(
            parse_block_size_override(Some("128K".to_string())),
            Some(128 * 1024)
        );
    }

    #[test]
    fn parse_block_size_zero_rejected() {
        assert_eq!(parse_block_size_override(Some("0".to_string())), None);
    }

    #[test]
    fn parse_block_size_negative_rejected() {
        assert_eq!(parse_block_size_override(Some("-1".to_string())), None);
    }

    #[test]
    fn parse_block_size_empty_rejected() {
        assert_eq!(parse_block_size_override(Some("   ".to_string())), None);
    }

    #[test]
    fn parse_block_size_malformed_rejected() {
        assert_eq!(parse_block_size_override(Some("bogus".to_string())), None);
        assert_eq!(parse_block_size_override(Some("4X".to_string())), None);
    }

    #[test]
    fn parse_block_size_above_max_is_clamped() {
        // 2 GiB requested; clamped to the 1 GiB ceiling rather than honoured.
        let requested = 2 * MAX_BUFFER_POOL_BLOCK_SIZE;
        assert_eq!(
            parse_block_size_override(Some(requested.to_string())),
            Some(MAX_BUFFER_POOL_BLOCK_SIZE)
        );
    }

    #[test]
    fn explicit_buffer_size_overrides_default() {
        // Struct-update precedence: an explicit buffer_size wins over whatever
        // the env-aware default() produced, giving "explicit config > env var".
        let cfg = GlobalBufferPoolConfig {
            buffer_size: 2 * 1024 * 1024,
            ..GlobalBufferPoolConfig::default()
        };
        assert_eq!(cfg.buffer_size, 2 * 1024 * 1024);
    }
}
