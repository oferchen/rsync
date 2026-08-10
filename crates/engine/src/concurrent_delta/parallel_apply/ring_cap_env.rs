//! Operator override for the per-file reorder-ring capacity (ROB-11, #3678).
//!
//! [`super::ParallelDeltaApplier`] sizes each per-file
//! [`super::ReorderBuffer`] with the hard default
//! [`super::ParallelDeltaApplier::DEFAULT_PER_FILE_REORDER_CAPACITY`] (64).
//! The ROB series (#3667) covers the case where adaptive sizing or a
//! workload-tuned cap would avoid normal-operation spill activations. Until
//! the adaptive path lands by default, operators need an escape hatch: a
//! single env var that pins the per-file ring capacity, bypassing the hard
//! 64 default and any future heuristic.
//!
//! This module mirrors the [`super::shard_sizing`] pattern: read the
//! `OC_RSYNC_REORDER_RING_CAP` env var once via a [`OnceLock`], parse it as
//! a positive `usize`, and expose the result through
//! [`resolve_ring_capacity`]. Mirrors the STN series convention
//! (`OC_RSYNC_SPILL_DIR` / `OC_RSYNC_SPILL_THRESHOLD_BYTES` /
//! `OC_RSYNC_SPILL_COMPRESSION`):
//!
//! - Read once at startup (process-wide [`OnceLock<Option<usize>>`]) so the
//!   env-var read never lands on a hot per-file construction path.
//! - Positive integers (`>= 1`) override the supplied default verbatim.
//! - Unparseable values emit a one-shot `eprintln!` warning and fall back to
//!   the supplied default; unparseable input is never silently ignored.
//! - Unset env var is the common case; [`resolve_ring_capacity`] returns the
//!   caller-supplied default with no allocation or syscall.
//!
//! The override has no upper clamp by design: operators who set
//! `OC_RSYNC_REORDER_RING_CAP=8192` to confirm a hypothesis about adversarial
//! workload reordering should get exactly 8192. The natural ceiling is
//! [`usize::MAX`], which would OOM the host before it could damage the
//! applier; the parser rejects `0` since a zero-capacity ring would panic
//! [`super::ReorderBuffer::new`] at construction time.

use std::sync::OnceLock;

/// Environment variable that pins the per-file reorder-ring capacity for
/// every [`super::ParallelDeltaApplier`] constructed in this process.
///
/// Accepts any positive integer parseable as [`usize`]. `0` and unparseable
/// values trigger a one-shot `eprintln!` warning and the applier falls back
/// to the caller-supplied default (typically
/// [`super::ParallelDeltaApplier::DEFAULT_PER_FILE_REORDER_CAPACITY`]).
///
/// Read once per process at first construction; subsequent reads return the
/// cached value without touching the environment again.
pub(super) const RING_CAP_ENV: &str = "OC_RSYNC_REORDER_RING_CAP";

/// Process-wide cache for the parsed override value.
///
/// `None` after the OnceLock initialiser runs means the env var was unset or
/// unparseable; callers fall back to their supplied default. `Some(n)` means
/// the env var was set to a valid positive integer `n` and every applier
/// will use `n` as the per-file ring capacity.
static OVERRIDE: OnceLock<Option<usize>> = OnceLock::new();

/// Resolves the per-file reorder-ring capacity for a fresh applier.
///
/// Returns the cached env-override when set to a positive integer; otherwise
/// returns `default`. Initialisation reads the environment at most once per
/// process; an unparseable or zero-valued env var emits a one-shot
/// `eprintln!` warning on the first call so operators learn about the typo
/// rather than silently inheriting the default.
#[must_use]
pub(super) fn resolve_ring_capacity(default: usize) -> usize {
    OVERRIDE.get_or_init(load_from_env).unwrap_or(default)
}

/// Reads [`RING_CAP_ENV`] from the environment and parses it.
///
/// Factored out so the [`OnceLock`] initialiser stays a single function
/// pointer. The eprintln warning fires at most once per process because
/// [`OnceLock::get_or_init`] guarantees the initialiser runs exactly once.
fn load_from_env() -> Option<usize> {
    let raw = std::env::var(RING_CAP_ENV).ok()?;
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return None;
    }
    match trimmed.parse::<usize>() {
        Ok(0) => {
            eprintln!(
                "oc-rsync: {RING_CAP_ENV}=0 is invalid (capacity must be positive); falling back to default"
            );
            None
        }
        Ok(n) => Some(n),
        Err(err) => {
            eprintln!(
                "oc-rsync: {RING_CAP_ENV}={raw:?} could not be parsed ({err}); falling back to default"
            );
            None
        }
    }
}

#[cfg(test)]
mod tests {
    //! Tests for the env-var parser run against [`load_from_env`] directly
    //! so the process-wide [`OVERRIDE`] cache never poisons sibling tests.
    //! The cache is exercised in the integration test at
    //! `crates/engine/tests/parallel_apply_ring_cap_env.rs` (single-shot,
    //! single-process).
    use super::*;
    use std::sync::Mutex;

    /// Serialises env mutation across this module's tests. `std::env::set_var`
    /// is process-wide; nextest runs tests in the same process, so concurrent
    /// env mutation would race. Mirrors the pattern documented on
    /// [`super::super::shard_sizing`].
    static ENV_MUTEX: Mutex<()> = Mutex::new(());

    /// RAII guard that captures the override's prior value on construction
    /// and restores it on drop, even if the test panics. Pairs with
    /// [`ENV_MUTEX`] so only one env-touching test runs at a time.
    struct EnvGuard {
        prev: Option<String>,
        _lock: std::sync::MutexGuard<'static, ()>,
    }

    impl EnvGuard {
        fn set(value: &str) -> Self {
            let lock = ENV_MUTEX.lock().unwrap_or_else(|p| p.into_inner());
            let prev = std::env::var(RING_CAP_ENV).ok();
            // SAFETY: process-wide env access serialised by ENV_MUTEX.
            unsafe {
                std::env::set_var(RING_CAP_ENV, value);
            }
            Self { prev, _lock: lock }
        }

        fn clear() -> Self {
            let lock = ENV_MUTEX.lock().unwrap_or_else(|p| p.into_inner());
            let prev = std::env::var(RING_CAP_ENV).ok();
            // SAFETY: see [`Self::set`].
            unsafe {
                std::env::remove_var(RING_CAP_ENV);
            }
            Self { prev, _lock: lock }
        }
    }

    impl Drop for EnvGuard {
        fn drop(&mut self) {
            // SAFETY: see [`Self::set`].
            unsafe {
                match self.prev.take() {
                    Some(prev) => std::env::set_var(RING_CAP_ENV, prev),
                    None => std::env::remove_var(RING_CAP_ENV),
                }
            }
        }
    }

    #[test]
    fn load_from_env_returns_none_when_unset() {
        let _guard = EnvGuard::clear();
        assert_eq!(load_from_env(), None);
    }

    #[test]
    fn load_from_env_parses_positive_integer() {
        let _guard = EnvGuard::set("128");
        assert_eq!(load_from_env(), Some(128));
    }

    #[test]
    fn load_from_env_accepts_very_large_value() {
        // No upper clamp: an operator pinning a deep ring for an adversarial
        // workload must get exactly the value they set. Memory exhaustion is
        // the natural ceiling; the parser only refuses zero.
        let _guard = EnvGuard::set("1048576");
        assert_eq!(load_from_env(), Some(1_048_576));
    }

    #[test]
    fn load_from_env_trims_whitespace() {
        let _guard = EnvGuard::set("  256  ");
        assert_eq!(load_from_env(), Some(256));
    }

    #[test]
    fn load_from_env_rejects_zero() {
        let _guard = EnvGuard::set("0");
        assert_eq!(load_from_env(), None);
    }

    #[test]
    fn load_from_env_rejects_empty_string() {
        let _guard = EnvGuard::set("");
        assert_eq!(load_from_env(), None);
    }

    #[test]
    fn load_from_env_rejects_negative() {
        let _guard = EnvGuard::set("-1");
        // `-1` fails `usize` parse; falls back to None (default).
        assert_eq!(load_from_env(), None);
    }

    #[test]
    fn load_from_env_rejects_non_numeric() {
        let _guard = EnvGuard::set("not-a-number");
        assert_eq!(load_from_env(), None);
    }

    #[test]
    fn load_from_env_rejects_trailing_garbage() {
        // `usize::parse` rejects "64x"; operator gets a typed warning rather
        // than a silently-truncated value.
        let _guard = EnvGuard::set("64x");
        assert_eq!(load_from_env(), None);
    }
}
