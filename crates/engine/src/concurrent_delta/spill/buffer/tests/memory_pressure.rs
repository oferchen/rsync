//! RSS-aware spill trigger tests
//! ([`SpillPolicy::memory_pressure_bytes`](super::super::super::policy::SpillPolicy::memory_pressure_bytes)).

use super::super::super::SpillableReorderBuffer;
use super::super::super::rss;
use super::drain_all;

#[test]
fn memory_pressure_default_none_does_not_trigger() {
    // No RSS knob configured: a buffer with a high byte budget must not
    // spill on inserts that stay well under that budget. This pins the
    // default behaviour and guards against accidental regressions where
    // the RSS path triggers even with the knob disabled.
    let mut buf: SpillableReorderBuffer<u64> = SpillableReorderBuffer::new(64, 1024);
    assert_eq!(buf.memory_pressure_bytes(), None);

    for i in 0..10 {
        buf.insert(i, i * 3).unwrap();
    }

    let stats = buf.spill_stats();
    assert_eq!(
        stats.spill_events, 0,
        "no spill must occur when both byte budget and RSS knob are slack"
    );
    let items = drain_all(&mut buf);
    assert_eq!(items.len(), 10);
}

#[test]
fn memory_pressure_threshold_forces_spill_when_exceeded() {
    // RSS threshold of 1 byte is guaranteed to be exceeded on every
    // platform whose probe returns a non-zero reading. The byte budget
    // is deliberately set to a value that the inserts never cross, so
    // any spill activity must come from the RSS path. Platforms whose
    // RSS probe returns zero or `Unsupported` skip the assertion - they
    // exercise the "no effect" contract instead.
    rss::invalidate_cache();
    let rss_probe = rss::current_rss_bytes();

    let mut buf: SpillableReorderBuffer<u64> =
        SpillableReorderBuffer::new(64, 1_000_000).with_memory_pressure_bytes(Some(1));

    for i in (0..6).rev() {
        buf.insert(i, i * 7).unwrap();
    }

    let stats = buf.spill_stats();
    if matches!(rss_probe, Ok(bytes) if bytes > 0) {
        assert!(
            stats.spill_events > 0,
            "RSS pressure must force a spill: stats={stats:?}, rss_probe={rss_probe:?}"
        );
    } else {
        // macOS stub (Ok(0)) and Windows Unsupported both keep the
        // byte-budget path in charge; with a 1 MB budget no spill is
        // expected.
        assert_eq!(
            stats.spill_events, 0,
            "RSS knob must be a no-op when the probe returns {rss_probe:?}"
        );
    }

    // Regardless of platform, every item must still drain correctly.
    let items = drain_all(&mut buf);
    let expected: Vec<u64> = (0..6).map(|i| i * 7).collect();
    assert_eq!(items, expected);
}

#[test]
fn memory_pressure_unsupported_platform_falls_back() {
    // On platforms where the RSS probe is unavailable (Windows) or
    // stubbed at zero (macOS), enabling the RSS knob must not change
    // anything: the byte budget continues to govern the spill decision.
    // This test runs everywhere; on Linux it asserts the natural
    // outcome (no spill because the budget is huge), and on other
    // platforms it asserts the contract that the knob has no effect.
    rss::invalidate_cache();
    let rss_probe = rss::current_rss_bytes();

    let mut buf: SpillableReorderBuffer<u64> =
        SpillableReorderBuffer::new(32, 8 * 1024).with_memory_pressure_bytes(Some(u64::MAX));

    for i in 0..8 {
        buf.insert(i, i).unwrap();
    }

    // With a u64::MAX RSS threshold, even Linux must not force a spill:
    // the cached RSS reading is overwhelmingly below the cap. And on
    // platforms whose probe returns zero or errors, the knob is inert
    // by construction.
    let stats = buf.spill_stats();
    assert_eq!(
        stats.spill_events, 0,
        "u64::MAX RSS cap must never trigger; probe={rss_probe:?}, stats={stats:?}"
    );

    let items = drain_all(&mut buf);
    assert_eq!(items, (0u64..8).collect::<Vec<_>>());
}
