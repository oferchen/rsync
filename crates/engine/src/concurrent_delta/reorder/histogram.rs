//! Bucketed histograms for reorder-buffer diagnostics.
//!
//! Two scales are exposed because the drain loop needs them with different
//! granularities:
//!
//! - [`HistogramStats::new_pow2`] groups counts into powers-of-two buckets
//!   (`1, 2, 4, 8, ..., 512, >=1024`). Used by the drain-batch-size histogram
//!   so operators can read off the typical contiguous-run length at a
//!   glance.
//! - [`HistogramStats::new_microseconds`] groups wall-clock durations into
//!   decimal-decade buckets (`<1, 1-10, 10-100, 100-1000, 1000-10000,
//!   >=10000` microseconds). Used by the drain-pause histogram to surface
//!   head-of-line stalls that correlate with `force_insert` events.
//!
//! Sampling is allocation-free and bumps a single `u64` counter per
//! observation; the histogram is owned by [`ReorderBuffer`](super::ReorderBuffer)
//! and copied out by [`Metrics`](super::Metrics) snapshots.

use std::time::Duration;

/// Bucket scale for a [`HistogramStats`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Scale {
    /// Powers-of-two buckets: `1, 2, 4, 8, 16, 32, 64, 128, 256, 512, >=1024`.
    Pow2,
    /// Decimal-decade microsecond buckets: `<1, 1-10, 10-100, 100-1000,
    /// 1000-10000, >=10000`.
    Microseconds,
}

/// Number of buckets in the pow-2 layout.
const POW2_BUCKETS: usize = 11;
/// Number of buckets in the microsecond layout.
const US_BUCKETS: usize = 6;
/// Maximum bucket count across all scales; sized for the largest layout.
const MAX_BUCKETS: usize = POW2_BUCKETS;

/// Bucketed counter histogram. Cheap to update, cheap to copy.
///
/// Each observation bumps exactly one bucket. The histogram is `Copy` so
/// callers can snapshot it through [`Metrics`](super::Metrics) without
/// taking a reference into the buffer.
#[derive(Debug, Clone, Copy)]
pub struct HistogramStats {
    scale: Scale,
    /// Per-bucket counters. Only the first `bucket_count(scale)` entries are
    /// meaningful; the remainder are kept at zero to satisfy `Copy`.
    buckets: [u64; MAX_BUCKETS],
}

impl Default for HistogramStats {
    fn default() -> Self {
        Self::new_pow2()
    }
}

impl PartialEq for HistogramStats {
    fn eq(&self, other: &Self) -> bool {
        self.scale == other.scale && self.buckets() == other.buckets()
    }
}

impl Eq for HistogramStats {}

impl HistogramStats {
    /// Creates a histogram with powers-of-two buckets (`1, 2, 4, ..., >=1024`).
    #[must_use]
    pub const fn new_pow2() -> Self {
        Self {
            scale: Scale::Pow2,
            buckets: [0; MAX_BUCKETS],
        }
    }

    /// Creates a histogram with microsecond decade buckets (`<1, 1-10,
    /// 10-100, 100-1000, 1000-10000, >=10000`).
    #[must_use]
    pub const fn new_microseconds() -> Self {
        Self {
            scale: Scale::Microseconds,
            buckets: [0; MAX_BUCKETS],
        }
    }

    /// Returns the number of meaningful buckets for the configured scale.
    #[must_use]
    pub const fn bucket_count(&self) -> usize {
        match self.scale {
            Scale::Pow2 => POW2_BUCKETS,
            Scale::Microseconds => US_BUCKETS,
        }
    }

    /// Returns the per-bucket counters in display order.
    ///
    /// For pow-2: `[1, 2, 4, 8, 16, 32, 64, 128, 256, 512, >=1024]`.
    /// For microseconds: `[<1, 1-10, 10-100, 100-1000, 1000-10000, >=10000]`.
    #[must_use]
    pub fn buckets(&self) -> &[u64] {
        &self.buckets[..self.bucket_count()]
    }

    /// Returns the upper-bound label for the `index`th bucket as a
    /// human-readable string. For the final overflow bucket the label
    /// reflects `>=<lower>`.
    #[must_use]
    pub fn bucket_label(&self, index: usize) -> &'static str {
        match self.scale {
            Scale::Pow2 => POW2_LABELS[index.min(POW2_BUCKETS - 1)],
            Scale::Microseconds => US_LABELS[index.min(US_BUCKETS - 1)],
        }
    }

    /// Returns the total number of samples observed.
    #[must_use]
    pub fn total_samples(&self) -> u64 {
        self.buckets().iter().sum()
    }

    /// Records a count observation (drain-batch size).
    ///
    /// Counts of zero are ignored - an empty drain produced no batch and
    /// should not skew the distribution.
    pub fn record_count(&mut self, count: usize) {
        if count == 0 {
            return;
        }
        debug_assert!(
            matches!(self.scale, Scale::Pow2),
            "record_count requires a pow-2 histogram",
        );
        let bucket = pow2_bucket(count);
        self.buckets[bucket] = self.buckets[bucket].saturating_add(1);
    }

    /// Records a duration observation (drain-iteration pause).
    pub fn record_duration(&mut self, dur: Duration) {
        debug_assert!(
            matches!(self.scale, Scale::Microseconds),
            "record_duration requires a microsecond histogram",
        );
        let micros = dur.as_micros();
        let bucket = us_bucket(micros);
        self.buckets[bucket] = self.buckets[bucket].saturating_add(1);
    }
}

/// Resolves the bucket index for a non-zero count under the pow-2 layout.
fn pow2_bucket(count: usize) -> usize {
    // count >= 1 by caller guarantee. Bucket k holds counts in [2^k, 2^(k+1)).
    // The final overflow bucket absorbs counts >= 2^(POW2_BUCKETS - 1) = 1024.
    let n = (count as u64).max(1);
    let leading = n.leading_zeros();
    let bits = 64 - leading; // floor(log2(n)) + 1
    let idx = (bits as usize).saturating_sub(1);
    idx.min(POW2_BUCKETS - 1)
}

/// Resolves the bucket index for a duration sample under the microsecond
/// decade layout.
fn us_bucket(micros: u128) -> usize {
    if micros < 1 {
        0
    } else if micros < 10 {
        1
    } else if micros < 100 {
        2
    } else if micros < 1_000 {
        3
    } else if micros < 10_000 {
        4
    } else {
        5
    }
}

const POW2_LABELS: [&str; POW2_BUCKETS] = [
    "1", "2", "4", "8", "16", "32", "64", "128", "256", "512", ">=1024",
];

const US_LABELS: [&str; US_BUCKETS] = [
    "<1us",
    "1-10us",
    "10-100us",
    "100-1000us",
    "1000-10000us",
    ">=10000us",
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pow2_buckets_match_design() {
        let mut h = HistogramStats::new_pow2();
        for v in [1usize, 2, 3, 4, 7, 8, 15, 16, 1023, 1024, 9_999_999] {
            h.record_count(v);
        }
        // 1 -> bucket 0 (1)
        // 2, 3 -> bucket 1 (2)
        // 4, 7 -> bucket 2 (4)
        // 8, 15 -> bucket 3 (8)
        // 16 -> bucket 4 (16)
        // 1023 -> bucket 9 (512..1024)
        // 1024, 9_999_999 -> bucket 10 (>=1024)
        let buckets = h.buckets();
        assert_eq!(buckets[0], 1, "bucket 1");
        assert_eq!(buckets[1], 2, "bucket 2");
        assert_eq!(buckets[2], 2, "bucket 4");
        assert_eq!(buckets[3], 2, "bucket 8");
        assert_eq!(buckets[4], 1, "bucket 16");
        assert_eq!(buckets[9], 1, "bucket 512");
        assert_eq!(buckets[10], 2, "bucket >=1024");
        assert_eq!(h.total_samples(), 11);
    }

    #[test]
    fn pow2_zero_is_ignored() {
        let mut h = HistogramStats::new_pow2();
        h.record_count(0);
        assert_eq!(h.total_samples(), 0);
    }

    #[test]
    fn microsecond_buckets_match_design() {
        let mut h = HistogramStats::new_microseconds();
        h.record_duration(Duration::from_nanos(500)); // <1us
        h.record_duration(Duration::from_micros(0)); // <1us
        h.record_duration(Duration::from_micros(5)); // 1-10us
        h.record_duration(Duration::from_micros(50)); // 10-100us
        h.record_duration(Duration::from_micros(500)); // 100-1000us
        h.record_duration(Duration::from_millis(5)); // 1000-10000us
        h.record_duration(Duration::from_millis(100)); // >=10000us
        let buckets = h.buckets();
        assert_eq!(buckets, [2, 1, 1, 1, 1, 1]);
        assert_eq!(h.total_samples(), 7);
    }

    #[test]
    fn labels_round_trip() {
        let pow2 = HistogramStats::new_pow2();
        assert_eq!(pow2.bucket_label(0), "1");
        assert_eq!(pow2.bucket_label(POW2_BUCKETS - 1), ">=1024");
        let us = HistogramStats::new_microseconds();
        assert_eq!(us.bucket_label(0), "<1us");
        assert_eq!(us.bucket_label(US_BUCKETS - 1), ">=10000us");
    }

    #[test]
    fn equality_compares_scale_and_buckets() {
        let a = HistogramStats::new_pow2();
        let b = HistogramStats::new_pow2();
        assert_eq!(a, b);
        let c = HistogramStats::new_microseconds();
        assert_ne!(a, c, "different scales must compare unequal");
    }
}
