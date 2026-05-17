//! Telemetry tests for [`RegisteredBufferStats`]: acquire/miss counters,
//! `miss_rate` math, and snapshot semantics.

use super::super::stats::RegisteredBufferStats;
use super::{try_group, try_ring};

/// A freshly created group reports zero acquires and zero misses.
#[test]
fn stats_initially_zero() {
    let Some(ring) = try_ring(4) else { return };
    let Some(group) = try_group(&ring, 4096, 2) else {
        return;
    };
    let stats = group.stats();
    assert_eq!(stats.total_acquires, 0);
    assert_eq!(stats.total_misses, 0);
    assert_eq!(stats.miss_rate(), 0.0);
}

/// Successful checkouts bump `total_acquires` but not `total_misses`.
#[test]
fn stats_count_successful_checkouts() {
    let Some(ring) = try_ring(4) else { return };
    let Some(group) = try_group(&ring, 4096, 4) else {
        return;
    };

    let s0 = group.checkout().expect("slot 0");
    let s1 = group.checkout().expect("slot 1");
    let stats = group.stats();
    assert_eq!(stats.total_acquires, 2);
    assert_eq!(stats.total_misses, 0);
    assert_eq!(stats.miss_rate(), 0.0);

    drop(s0);
    drop(s1);
}

/// `checkout` returning `None` increments both `total_acquires` and
/// `total_misses`, and `miss_rate` reflects the ratio.
#[test]
fn stats_count_misses_on_exhaustion() {
    let Some(ring) = try_ring(4) else { return };
    let Some(group) = try_group(&ring, 4096, 2) else {
        return;
    };

    // Exhaust the pool.
    let _s0 = group.checkout().expect("slot 0");
    let _s1 = group.checkout().expect("slot 1");

    // Three forced misses.
    assert!(group.checkout().is_none());
    assert!(group.checkout().is_none());
    assert!(group.checkout().is_none());

    let stats = group.stats();
    assert_eq!(stats.total_acquires, 5);
    assert_eq!(stats.total_misses, 3);
    let mr = stats.miss_rate();
    assert!(
        (mr - 3.0 / 5.0).abs() < 1e-12,
        "expected miss_rate=0.6, got {mr}"
    );
}

/// Returning a slot does not affect telemetry counters: `total_acquires`
/// is the lifetime acquire count, never decremented.
#[test]
fn stats_not_decremented_on_return() {
    let Some(ring) = try_ring(4) else { return };
    let Some(group) = try_group(&ring, 4096, 2) else {
        return;
    };

    let s = group.checkout().expect("slot");
    drop(s);
    assert_eq!(group.stats().total_acquires, 1);
    assert_eq!(group.stats().total_misses, 0);

    // Re-acquire the same slot bumps acquires again.
    let s = group.checkout().expect("slot reacquired");
    assert_eq!(group.stats().total_acquires, 2);
    drop(s);
}

/// `RegisteredBufferStats::miss_rate` returns 0.0 when no acquires have
/// been recorded, matching the `BufferPoolStats::hit_rate` convention.
#[test]
fn stats_miss_rate_zero_when_no_acquires() {
    let s = RegisteredBufferStats {
        total_acquires: 0,
        total_misses: 0,
    };
    assert_eq!(s.miss_rate(), 0.0);
}

/// `miss_rate` is exactly 1.0 when every acquire missed.
#[test]
fn stats_miss_rate_all_misses() {
    let s = RegisteredBufferStats {
        total_acquires: 7,
        total_misses: 7,
    };
    assert!((s.miss_rate() - 1.0).abs() < 1e-12);
}

/// `RegisteredBufferStats` is `Copy`, so a snapshot taken before the
/// group is dropped remains usable after. This documents that
/// telemetry consumers (e.g., the adaptive sizer) do not need to
/// outlive their group via lifetime coupling.
#[test]
fn stats_snapshot_survives_group_drop() {
    let Some(ring) = try_ring(4) else { return };
    let Some(group) = try_group(&ring, 4096, 2) else {
        return;
    };

    // Generate measurable telemetry: one hit, one forced miss.
    let s = group.checkout().expect("slot");
    let s2 = group.checkout().expect("second slot");
    assert!(group.checkout().is_none());
    drop(s);
    drop(s2);

    let snapshot = group.stats();
    assert_eq!(snapshot.total_acquires, 3);
    assert_eq!(snapshot.total_misses, 1);

    // Drop the group. The snapshot is plain integers and must remain
    // observably identical after the source group is gone.
    drop(group);

    assert_eq!(snapshot.total_acquires, 3);
    assert_eq!(snapshot.total_misses, 1);
    let mr = snapshot.miss_rate();
    assert!(
        (mr - 1.0 / 3.0).abs() < 1e-12,
        "miss_rate snapshot drift: {mr}"
    );
}
