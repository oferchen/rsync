//! Write chunk-size calculation for bandwidth-limited transfers.
//!
//! The maximum chunk size scales linearly with the configured rate, keeping
//! I/O granularity proportional to throughput. This mirrors upstream
//! `options.c:2395` where `bwlimit_writemax = bwlimit * 128` with a floor
//! of 512 bytes so that pacing sleeps remain short and responsive.

use std::num::NonZeroU64;

use super::super::MIN_WRITE_MAX;

/// Calculates the maximum chunk size for a given rate limit.
///
/// The write-max scales linearly with KiB of bandwidth, clamped to at least
/// `MIN_WRITE_MAX`.
/// upstream: options.c:2395-2397 - bwlimit_writemax = bwlimit * 128, min 512
pub(super) fn calculate_write_max(limit: NonZeroU64) -> usize {
    let kib = if limit.get() < 1024 {
        1
    } else {
        limit.get() / 1024
    };

    let base_write_max = u128::from(kib)
        .saturating_mul(128)
        .max(MIN_WRITE_MAX as u128);
    let write_max = base_write_max.min(usize::MAX as u128) as usize;

    write_max.max(MIN_WRITE_MAX)
}
