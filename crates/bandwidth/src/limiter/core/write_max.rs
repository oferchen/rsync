//! Write chunk-size calculation for bandwidth-limited transfers.
//!
//! The maximum chunk size scales linearly with the configured rate, keeping
//! I/O granularity proportional to throughput. This mirrors the approach in
//! upstream rsync's `io.c` where the write buffer is sized relative to the
//! `--bwlimit` value so that pacing sleeps remain short and responsive.

use std::num::NonZeroU64;

use super::super::MIN_WRITE_MAX;

/// Calculates the maximum chunk size for a given rate limit and optional burst.
///
/// The base write-max scales linearly with KiB of bandwidth, clamped to at
/// least `MIN_WRITE_MAX`. When a burst override is present it replaces the
/// calculated value (still respecting the minimum).
pub(super) fn calculate_write_max(limit: NonZeroU64, burst: Option<NonZeroU64>) -> usize {
    let kib = if limit.get() < 1024 {
        1
    } else {
        limit.get() / 1024
    };

    let base_write_max = u128::from(kib)
        .saturating_mul(128)
        .max(MIN_WRITE_MAX as u128);
    let mut write_max = base_write_max.min(usize::MAX as u128) as usize;

    if let Some(burst) = burst {
        let burst = burst.get().min(usize::MAX as u64);
        write_max = usize::try_from(burst)
            .unwrap_or(usize::MAX)
            .max(MIN_WRITE_MAX)
            .max(1);
    }

    write_max.max(MIN_WRITE_MAX)
}
