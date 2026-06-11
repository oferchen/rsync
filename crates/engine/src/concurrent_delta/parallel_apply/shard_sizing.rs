//! Adaptive DashMap shard-count sizing for [`super::ParallelDeltaApplier`]
//! (DMC-CON.2/.3, #3996/#3997).
//!
//! DashMap's default constructor picks `available_parallelism() * 4` rounded
//! up to a power of two. That target tracks the host's CPU count, not the
//! applier's actual worker concurrency. At million-file workloads the
//! per-op shard-selection overhead starts to matter, while at low worker
//! counts the over-large shard table simply wastes memory and L1d capacity
//! for shard `RwLock` headers nobody contends.
//!
//! This module computes a shard count from the applier's `worker_count`:
//!
//! ```text
//! shard_count = (worker_count * 4).next_power_of_two().clamp(4, 1024)
//! ```
//!
//! - `worker_count = 1`  -> 4 shards (clamped from 4).
//! - `worker_count = 4`  -> 16 shards.
//! - `worker_count = 16` -> 64 shards.
//! - `worker_count = 64` -> 256 shards.
//! - `worker_count >= 256` -> 1024 shards (clamped cap).
//!
//! The lower bound of 4 keeps DashMap operational when callers pass
//! `worker_count = 0` (the "use ambient rayon pool" sentinel documented on
//! [`super::ParallelDeltaApplier::new`]). The upper bound of 1024 caps the
//! per-shard fixed cost (`RwLock` + empty `HashMap` ~= 72 bytes per shard,
//! so 1024 shards is ~72 KiB - the same order of magnitude DashMap allocates
//! at default settings on a 128-core host).
//!
//! The operator override `OC_RSYNC_DASHMAP_SHARDS` skips the heuristic when
//! set to a valid `usize`. Invalid values fall back to the heuristic so a
//! mistyped environment never silently breaks the applier.

/// Lower bound on the computed shard count.
///
/// Matches DashMap's documented minimum useful shard count: with fewer than
/// four shards the per-op hash + modulus overhead approaches the cost of a
/// single `Mutex<HashMap>` while losing the partitioning benefit entirely.
pub(super) const MIN_SHARDS: usize = 4;

/// Upper bound on the computed shard count.
///
/// Caps the per-shard fixed cost so a malformed `worker_count` (e.g.
/// `usize::MAX` from a future API mistake) cannot allocate an unbounded
/// shard table. 1024 shards keeps the empty-map overhead under ~72 KiB,
/// matching the order of magnitude DashMap's own default produces on a
/// 128-core host (`128 * 4 = 512` shards rounded to the next power of two).
pub(super) const MAX_SHARDS: usize = 1024;

/// Environment variable that overrides the heuristic.
///
/// Set to a positive integer to pin the shard count; any parse failure or
/// zero value falls back to [`default_shard_count`] so a mistyped override
/// never breaks the applier. Useful for micro-benchmarks, A/B comparisons
/// against the BR-3j.f / DMB.b sweeps, and production tuning on hosts where
/// the sequential `FileNdx` distribution clusters into a subset of shards
/// (see `docs/design/dmc-con-adaptive-sharding.md`).
pub(super) const SHARDS_ENV: &str = "OC_RSYNC_DASHMAP_SHARDS";

/// Computes the default shard count for a `ParallelDeltaApplier` configured
/// with `worker_count` rayon workers.
///
/// The heuristic is `(worker_count * 4).next_power_of_two().clamp(MIN, MAX)`
/// with `MIN = 4` and `MAX = 1024`. See the module docs for the rationale
/// and the per-`worker_count` table.
#[must_use]
pub(super) fn default_shard_count(worker_count: usize) -> usize {
    // Saturating multiply so `worker_count == usize::MAX` cannot wrap to a
    // tiny value. The subsequent `clamp` then caps at MAX_SHARDS, so the
    // exact pre-clamp value past the cap does not matter.
    let raw = worker_count.saturating_mul(4);
    // `next_power_of_two` panics on overflow; saturate to MAX_SHARDS in
    // that case so we never panic from a configuration value.
    let rounded = raw.checked_next_power_of_two().unwrap_or(MAX_SHARDS);
    rounded.clamp(MIN_SHARDS, MAX_SHARDS)
}

/// Resolves the shard count for a fresh applier, honouring the operator
/// override when present.
///
/// Reads [`SHARDS_ENV`] via [`std::env::var`]. A valid positive integer is
/// clamped to `[MIN_SHARDS, MAX_SHARDS]` and rounded up to the next power
/// of two (DashMap 6.1 panics on non-power-of-two shard counts at
/// construction time). Any parse failure or unset variable falls back to
/// [`default_shard_count`].
#[must_use]
pub(super) fn resolve_shard_count(worker_count: usize) -> usize {
    match std::env::var(SHARDS_ENV) {
        Ok(raw) => match raw.trim().parse::<usize>() {
            Ok(0) | Err(_) => default_shard_count(worker_count),
            Ok(n) => {
                let clamped = n.clamp(MIN_SHARDS, MAX_SHARDS);
                // DashMap::with_shard_amount panics on non-power-of-two
                // counts. Round up so an operator-provided 42 lands at
                // 64 instead of crashing the applier at boot.
                clamped
                    .checked_next_power_of_two()
                    .unwrap_or(MAX_SHARDS)
                    .min(MAX_SHARDS)
            }
        },
        Err(_) => default_shard_count(worker_count),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    /// Serialises the env-mutating tests below. `std::env::set_var` is
    /// process-wide; nextest runs tests in the same process by default,
    /// so concurrent env mutation would race. This mutex is the same
    /// pattern other engine env-touching tests use (see
    /// `crates/engine/tests/buffer_pool_env_*.rs`).
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
            let prev = std::env::var(SHARDS_ENV).ok();
            // SAFETY: process-wide env access serialised by ENV_MUTEX.
            unsafe {
                std::env::set_var(SHARDS_ENV, value);
            }
            Self { prev, _lock: lock }
        }

        fn clear() -> Self {
            let lock = ENV_MUTEX.lock().unwrap_or_else(|p| p.into_inner());
            let prev = std::env::var(SHARDS_ENV).ok();
            // SAFETY: see [`Self::set`].
            unsafe {
                std::env::remove_var(SHARDS_ENV);
            }
            Self { prev, _lock: lock }
        }
    }

    impl Drop for EnvGuard {
        fn drop(&mut self) {
            // SAFETY: see [`Self::set`].
            unsafe {
                match self.prev.take() {
                    Some(prev) => std::env::set_var(SHARDS_ENV, prev),
                    None => std::env::remove_var(SHARDS_ENV),
                }
            }
        }
    }

    #[test]
    fn default_shard_count_clamps_to_min_for_low_worker_count() {
        // worker_count=0 (ambient rayon pool sentinel) and worker_count=1
        // both round to 4 (the MIN clamp). worker_count*4 = 0 or 4;
        // next_power_of_two leaves them at 1 / 4 respectively, then the
        // MIN clamp lifts the 1 to 4.
        assert_eq!(default_shard_count(0), MIN_SHARDS);
        assert_eq!(default_shard_count(1), MIN_SHARDS);
    }

    #[test]
    fn default_shard_count_matches_table_for_typical_worker_counts() {
        // Spec table from the module docs. Each cell is
        // (worker_count * 4).next_power_of_two() with no clamp activity.
        assert_eq!(default_shard_count(4), 16);
        assert_eq!(default_shard_count(8), 32);
        assert_eq!(default_shard_count(16), 64);
        assert_eq!(default_shard_count(32), 128);
        assert_eq!(default_shard_count(64), 256);
    }

    #[test]
    fn default_shard_count_rounds_up_to_power_of_two() {
        // worker_count=3 -> 12 -> next_power_of_two = 16.
        // worker_count=5 -> 20 -> next_power_of_two = 32.
        // worker_count=17 -> 68 -> next_power_of_two = 128.
        assert_eq!(default_shard_count(3), 16);
        assert_eq!(default_shard_count(5), 32);
        assert_eq!(default_shard_count(17), 128);
    }

    #[test]
    fn default_shard_count_clamps_to_max_for_huge_worker_count() {
        // 256 workers -> 1024 (still under the cap).
        assert_eq!(default_shard_count(256), MAX_SHARDS);
        // Above 256 the raw value would exceed the cap; the clamp
        // pins it to MAX_SHARDS. `usize::MAX` exercises the
        // saturating_mul + checked_next_power_of_two path.
        assert_eq!(default_shard_count(1024), MAX_SHARDS);
        assert_eq!(default_shard_count(usize::MAX), MAX_SHARDS);
    }

    #[test]
    fn resolve_shard_count_uses_env_override_when_set() {
        let _guard = EnvGuard::set("64");
        // 64 is in [MIN, MAX] and a power of two so it is used verbatim,
        // bypassing the heuristic that would have returned 16 for
        // worker_count=4. The DashMap API accepts only power-of-two
        // shard counts at construction time; the override doc string
        // mirrors that constraint.
        assert_eq!(resolve_shard_count(4), 64);
    }

    #[test]
    fn resolve_shard_count_rounds_env_override_to_power_of_two() {
        let _guard = EnvGuard::set("42");
        // DashMap 6.1 panics on non-power-of-two shard counts. The
        // resolver rounds up so an operator-provided 42 lands at 64
        // instead of crashing the applier at boot.
        assert_eq!(resolve_shard_count(4), 64);
    }

    #[test]
    fn resolve_shard_count_clamps_env_override_to_max() {
        let _guard = EnvGuard::set("999999");
        // Override above MAX clamps down so DashMap construction never
        // sees a pathological value.
        assert_eq!(resolve_shard_count(4), MAX_SHARDS);
    }

    #[test]
    fn resolve_shard_count_clamps_env_override_to_min() {
        let _guard = EnvGuard::set("2");
        // Override below MIN clamps up, preserving DashMap's minimum
        // useful shard count.
        assert_eq!(resolve_shard_count(8), MIN_SHARDS);
    }

    #[test]
    fn resolve_shard_count_falls_back_on_invalid_env() {
        let _guard = EnvGuard::set("not-a-number");
        // Parse failure falls back to the heuristic for worker_count=4.
        assert_eq!(resolve_shard_count(4), default_shard_count(4));
    }

    #[test]
    fn resolve_shard_count_falls_back_on_zero_env() {
        let _guard = EnvGuard::set("0");
        // Zero is treated as invalid (it would mean "no shards", which
        // DashMap cannot represent) and falls back to the heuristic.
        assert_eq!(resolve_shard_count(8), default_shard_count(8));
    }

    #[test]
    fn resolve_shard_count_uses_heuristic_when_env_unset() {
        let _guard = EnvGuard::clear();
        assert_eq!(resolve_shard_count(4), default_shard_count(4));
        assert_eq!(resolve_shard_count(64), default_shard_count(64));
    }

    #[test]
    fn resolve_shard_count_trims_whitespace() {
        let _guard = EnvGuard::set("  64  ");
        // Trim handles operators who export with trailing newlines or
        // leading spaces from shell history.
        assert_eq!(resolve_shard_count(4), 64);
    }
}
