//! Long-running BGID lifecycle stress test (#2297).
//!
//! Simulates 100,000 sequential mock daemon sessions, each acquiring a
//! buffer-group id from [`fast_io::BgidAllocator`] and immediately returning
//! it on drop. Confirms that the free-list-first recycling policy keeps the
//! 16-bit bgid namespace bounded - a long-running daemon must not leak ids
//! into the namespace and silently collide once `NEXT_BGID` wraps past
//! `u16::MAX`.
//!
//! Gated twice:
//!
//! 1. `cfg(all(target_os = "linux", feature = "io_uring"))` - the bgid
//!    allocator only ships on the Linux io_uring backend.
//! 2. `OC_RSYNC_BGID_STRESS=1` - the loop runs 100,000 acquire/release
//!    cycles. The work is cheap (no kernel registration, no I/O) but the
//!    iteration count alone would dominate the default test budget on
//!    constrained runners, so the assertion is opt-in.
//!
//! Without the env var the test exits cleanly with a single-line skip
//! message so the default `cargo nextest run` stays fast.
//!
//! Run it explicitly with:
//!
//! ```text
//! OC_RSYNC_BGID_STRESS=1 cargo nextest run -p daemon \
//!     --test bgid_long_running_stress --all-features
//! ```

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

/// Env-var trigger. Unset = skip with a clear message.
const STRESS_ENV: &str = "OC_RSYNC_BGID_STRESS";

#[test]
fn one_hundred_thousand_sessions_do_not_leak_bgids() {
    if std::env::var_os(STRESS_ENV).is_none() {
        eprintln!(
            "[bgid-stress] skipped: set {STRESS_ENV}=1 to run the 100K session lifecycle stress"
        );
        return;
    }

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
