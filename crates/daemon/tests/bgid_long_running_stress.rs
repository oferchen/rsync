//! Long-running BGID lifecycle stress test (#2297).
//!
//! Simulates 100,000 sequential mock daemon sessions, each acquiring a
//! buffer-group id from [`fast_io::BgidAllocator`] and immediately returning
//! it on drop. Confirms that the free-list-first recycling policy keeps the
//! 16-bit bgid namespace bounded - a long-running daemon must not leak ids
//! into the namespace and silently collide once `NEXT_BGID` wraps past
//! `u16::MAX`.
//!
//! Gated once, on `cfg(target_os = "linux")`: the bgid allocator only ships
//! on the Linux io_uring backend. The Linux-only dev-dependency in
//! `Cargo.toml` pulls `fast_io` with the `io_uring` feature, so the real
//! allocator - not the always-erroring stub - backs this test. The gate is
//! deliberately NOT `feature = "io_uring"`: that would resolve against the
//! `daemon` crate's own features, which define no such feature, and would
//! silently compile the test out entirely.
//!
//! The loop runs unconditionally. 100,000 acquire/release cycles cost ~17 ms
//! measured (no kernel registration, no I/O), so there is no test-budget
//! reason to make the assertion opt-in. It previously sat behind
//! `OC_RSYNC_BGID_STRESS=1`, which no workflow ever set - the leak assertion
//! therefore never ran in CI while the test still reported a pass.

#![cfg(target_os = "linux")]

use fast_io::{BgidAllocator, bgid_inflight, bgid_peak_used};

/// Number of mock session lifecycles to exercise.
///
/// Sized to exceed the 16-bit bgid namespace (65,536) by ~50 % so an
/// implementation that leaks ids - or one that recycles only after a long
/// grace period - exhausts the allocator and the test fails with a clear
/// `BgidExhausted` rather than a slow leak.
const SESSION_CYCLES: u32 = 100_000;

/// Upper bound on the high-water mark across the entire run.
///
/// Each iteration releases its id before the next allocation, so the steady
/// state in-flight count is 1. A bound of 1,024 leaves generous headroom
/// for any prior test in the same binary that nudged `PEAK_USED` while
/// still failing loudly if recycling regresses (peak would otherwise climb
/// toward `SESSION_CYCLES`).
const PEAK_BGID_CEILING: u16 = 1_024;

#[test]
fn one_hundred_thousand_sessions_do_not_leak_bgids() {
    let peak_before = bgid_peak_used();
    let inflight_before = bgid_inflight();
    assert_eq!(
        inflight_before, 0,
        "test must start with no allocator-owned ids in flight, found {inflight_before}",
    );

    // Sequential acquire/release pairs. Each iteration models a daemon
    // session that holds exactly one buffer-group id for the lifetime of
    // its transfer and returns it on drop. With recycling enabled the
    // free-list immediately replays the id, so `NEXT_BGID` never advances
    // past 1 + `peak_before` and `BgidExhausted` is impossible.
    for cycle in 0..SESSION_CYCLES {
        let bgid = BgidAllocator::allocate().unwrap_or_else(|err| {
            panic!(
                "bgid allocation failed at cycle {cycle}/{SESSION_CYCLES}: {err:?} \
                 (peak_used={}, inflight={})",
                bgid_peak_used(),
                bgid_inflight(),
            )
        });
        BgidAllocator::deallocate(bgid);
    }

    let peak_after = bgid_peak_used();
    let inflight_after = bgid_inflight();

    assert_eq!(
        inflight_after, 0,
        "every cycle released its id; in-flight must be zero, got {inflight_after}",
    );

    // Peak must stay bounded. Without recycling, peak would equal the
    // number of fresh allocations (up to namespace exhaustion); with
    // recycling, peak only grows by the increment between consecutive
    // unreturned holds, which the single-thread acquire-then-release loop
    // pins to 1 above any pre-existing baseline.
    assert!(
        peak_after <= PEAK_BGID_CEILING,
        "peak bgid occupancy {peak_after} exceeded ceiling {PEAK_BGID_CEILING} after \
         {SESSION_CYCLES} acquire/release cycles (peak_before={peak_before}); free-list \
         recycling regressed",
    );

    // Reuse must dominate fresh allocation. The free-list answered all but
    // at most `peak_after` requests; the remainder were minted from
    // `NEXT_BGID`. Asserting `reused >> fresh` translates to
    // `SESSION_CYCLES - peak_after >> peak_after`, i.e. fresh allocations
    // account for a vanishing fraction of the workload.
    let fresh_upper_bound = u32::from(peak_after);
    let reused_lower_bound = SESSION_CYCLES.saturating_sub(fresh_upper_bound);
    assert!(
        reused_lower_bound >= fresh_upper_bound.saturating_mul(10),
        "free-list reuse must dominate: reused>={reused_lower_bound}, fresh<={fresh_upper_bound} \
         over {SESSION_CYCLES} cycles",
    );

    eprintln!(
        "[bgid-stress] {SESSION_CYCLES} cycles ok: peak_before={peak_before} peak_after={peak_after} \
         inflight={inflight_after} fresh<={fresh_upper_bound} reused>={reused_lower_bound}"
    );
}
